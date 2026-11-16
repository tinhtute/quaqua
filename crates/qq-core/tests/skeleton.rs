//! T37: one task from config seed to a stored run record, through the real
//! daemon, the worker flow and FakeHerdr.

use std::sync::Arc;
use std::time::{Duration, Instant};

use qq_core::config;
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::db;
use qq_core::driver::Registry;
use qq_core::herdr::FakeHerdr;
use qq_core::rpc::{self, Request};
use qq_core::run;
use qq_core::task::spawn::{spawn, Role, SpawnSpec};
use qq_core::task::worker_flow::{self, Deps};
use qq_core::testkit::TestWorkspace;
use rusqlite::Connection;
use serde_json::{json, Value};

const SHA: &str = "0123abc4567def890123abc4567def890123abcd";

fn template() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../templates/workspace/team.toml");
    std::fs::read_to_string(p)
        .expect("read template team.toml")
        .replace("harness = \"omp\"", "harness = \"claude\"")
}

/// A seeded workspace with one project and one task.
fn seeded(ws: &TestWorkspace) -> (Connection, i64) {
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    config::seed(&mut conn, &template()).expect("seed");
    conn.execute("INSERT INTO project(slug, created_ms) VALUES ('p', 1)", [])
        .unwrap();
    conn.execute(
        "INSERT INTO task(project_id, title, kind, disposition, state, created_ms) \
         VALUES (1, 't', 'ni', 'engineer', 'open', 1)",
        [],
    )
    .unwrap();
    (conn, 1)
}

fn start_daemon(ws: &TestWorkspace, herdr: Arc<FakeHerdr>) -> DaemonHandle {
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone(),
    });
    worker_flow::register(
        &mut d,
        Deps {
            herdr,
            runs_dir: ws.runs_dir(),
            drivers: Registry::native(),
        },
    )
    .expect("register worker flow");
    d.spawn().expect("daemon starts")
}

fn rpc_ok(ws: &TestWorkspace, verb: &str, args: Value) -> Value {
    rpc::call(&rpc::socket_path(&ws.runs_dir()), &Request::new(verb, args))
        .expect("rpc transport")
        .into_result()
        .unwrap_or_else(|e| panic!("{verb} refused: {e}"))
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn journal_rows(conn: &Connection, id: &str) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM events WHERE workflow_id = ?1",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

fn start_task(ws: &TestWorkspace, name: &str, timeout_ms: u64) -> Value {
    rpc_ok(
        ws,
        "task.start",
        json!({
            "name": name, "kind": "ni", "role": "peer", "pane": "p1",
            "prompt": "do the thing", "report": format!("{name}.md"),
            "timeout_ms": timeout_ms, "base": "main",
            "project_id": 1, "task_id": 1,
        }),
    )
}

#[test]
fn walking_skeleton_spawn_report_run_record() {
    let ws = TestWorkspace::new("skeleton");
    let (conn, _) = seeded(&ws);
    let herdr = Arc::new(FakeHerdr::new());
    let daemon = start_daemon(&ws, herdr.clone());

    let started = start_task(&ws, "w1", 60_000);
    assert_eq!(started["workflow"], "worker:w1");
    // Repeating the verb while the flow runs converges: no second agent,
    // run or prompt.
    let again = start_task(&ws, "w1", 60_000);
    assert_eq!(again["agent_id"], started["agent_id"]);

    // The prompt reaches the worker, then the worker publishes its report.
    wait_for("the prompt", || !herdr.prompts().is_empty());
    assert_eq!(
        herdr.prompts(),
        vec![("w1".to_string(), "do the thing".to_string())]
    );
    std::fs::write(
        ws.runs_dir().join("w1.md"),
        format!("Outcome: COMPLETE\nVerified: unit-test-verified {SHA}\n\n## Blockers\nnone\n"),
    )
    .unwrap();

    wait_for("the run to close", || {
        run::by_workflow(&conn, "worker:w1")
            .unwrap()
            .is_some_and(|r| r.ended_ms.is_some())
    });
    let r = run::by_workflow(&conn, "worker:w1").unwrap().unwrap();
    assert_eq!(
        (r.agent.as_str(), r.role.as_str(), r.kind.as_str()),
        ("w1", "peer", "ni")
    );
    assert_eq!(
        (r.harness.as_str(), r.model.as_str()),
        ("claude", "opus-5.5")
    );
    assert_eq!(r.effort.as_deref(), Some("low"));
    assert_eq!(r.outcome.as_deref(), Some("verified"));
    assert_eq!(r.candidate_sha.as_deref(), Some(SHA));
    assert_eq!((r.cost_state.as_str(), r.cost_usd), ("unknown", None));
    assert_eq!(
        herdr.start_args("w1"),
        ["--model", "claude-opus-5-5", "--effort", "low"]
    );

    // Calling the verb again after the end changes nothing.
    assert_eq!(start_task(&ws, "w1", 60_000)["finished"], true);
    let runs: i64 = conn
        .query_row("SELECT count(*) FROM run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 1);
    let agents: i64 = conn
        .query_row("SELECT count(*) FROM agent", [], |r| r.get(0))
        .unwrap();
    assert_eq!(agents, 1);
    assert_eq!(herdr.prompts().len(), 1);
    daemon.stop().unwrap();
}

#[test]
fn walking_skeleton_silent_worker_times_out() {
    let ws = TestWorkspace::new("skeleton-timeout");
    let (conn, _) = seeded(&ws);
    let herdr = Arc::new(FakeHerdr::new());
    let daemon = start_daemon(&ws, herdr);
    start_task(&ws, "w2", 300);
    wait_for("the run to close", || {
        run::by_workflow(&conn, "worker:w2")
            .unwrap()
            .is_some_and(|r| r.ended_ms.is_some())
    });
    let r = run::by_workflow(&conn, "worker:w2").unwrap().unwrap();
    assert_eq!(r.outcome.as_deref(), Some("timeout"));
    assert_eq!(r.candidate_sha, None);
    daemon.stop().unwrap();
}

#[test]
fn daemon_idle_worker_adds_no_poll_events() {
    let ws = TestWorkspace::new("skeleton-idle");
    let (conn, _) = seeded(&ws);
    let herdr = Arc::new(FakeHerdr::new());
    let daemon = start_daemon(&ws, herdr.clone());
    start_task(&ws, "w3", 60_000);
    wait_for("the prompt", || !herdr.prompts().is_empty());

    // Let the first status signal and the watchers settle, then watch the
    // journal across several watcher re-arm cycles with the worker unchanged.
    std::thread::sleep(Duration::from_millis(500));
    let before = journal_rows(&conn, "worker:w3");
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(
        journal_rows(&conn, "worker:w3"),
        before,
        "polling wrote rows"
    );

    // The same journal does grow when the worker's status changes: exactly
    // one signal, plus the decision the flow journals for it.
    let signals = || -> i64 {
        conn.query_row(
            "SELECT count(*) FROM events WHERE workflow_id = 'worker:w3' AND kind = 'signal'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let signals_before = signals();
    herdr.set_status("w3", "blocked");
    wait_for("the status signal", || signals() > signals_before);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(signals(), signals_before + 1);
    daemon.stop().unwrap();
}

#[test]
fn lead_and_peer_share_spawn_path() {
    let ws = TestWorkspace::new("skeleton-spawn");
    let (conn, _) = seeded(&ws);
    let herdr = FakeHerdr::new();
    let drivers = Registry::native();
    let go = |name: &str, role: Role, pane: &str| {
        spawn(
            &conn,
            &herdr,
            &drivers,
            &SpawnSpec {
                name,
                kind: "ni",
                role,
                pane,
                project_id: Some(1),
                spawner_id: None,
            },
            1,
        )
        .expect("spawn")
    };
    let lead = go("lead-a", Role::Lead, "pane-1");
    let peer = go("peer-a", Role::Peer, "pane-2");
    assert_eq!(lead.harness, peer.harness);
    assert_eq!((lead.model, lead.effort), (peer.model, peer.effort));
    assert_eq!(herdr.start_args("lead-a"), herdr.start_args("peer-a"));
    assert!(!herdr.start_args("lead-a").is_empty());
    let role = |n: &str| -> String {
        conn.query_row("SELECT role FROM agent WHERE name = ?1", [n], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        (role("lead-a").as_str(), role("peer-a").as_str()),
        ("lead", "peer")
    );

    // A repeat converges on the same agent row.
    let again = go("peer-a", Role::Peer, "pane-2");
    assert_eq!(again.agent_id, peer.agent_id);
}
