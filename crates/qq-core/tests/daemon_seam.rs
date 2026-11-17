//! T42b: the daemon seam observed through the RPC door: watches survive a
//! restart, late workflows hear pending mail, and mail and spawns obey the
//! topology edges.

use std::sync::Arc;
use std::time::{Duration, Instant};

use qq_core::authority;
use qq_core::bus;
use qq_core::cli::Registry as CliRegistry;
use qq_core::config;
use qq_core::daemon::supervisor::{Factory, Machine};
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::db;
use qq_core::driver::Registry as Drivers;
use qq_core::engine::journal::Journal;
use qq_core::engine::journal_sqlite::SqliteJournal;
use qq_core::engine::{
    Activity, ActivityError, ActivityRequest, ActivityResult, StepDecision, Workflow,
    WorkflowContext, WorkflowEvent,
};
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::rpc::{self, Request, Response};
use qq_core::task::worker_flow::{self, Deps};
use qq_core::testkit::TestWorkspace;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Records every Signal as `source/name`; never ends.
struct Recorder;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Heard(Vec<String>);

impl Workflow for Recorder {
    type Input = Value;
    type State = Heard;
    type Output = usize;

    fn step(
        &self,
        state: &mut Heard,
        _: &mut WorkflowContext,
        event: Option<WorkflowEvent>,
    ) -> StepDecision<usize> {
        if let Some(WorkflowEvent::Signal { source, name, .. }) = event {
            state.0.push(format!("{source}/{name}"));
        }
        StepDecision::Continue
    }
}

struct NoActivities;

impl Activity for NoActivities {
    fn execute(&self, _: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        Err(ActivityError::ExecutionFailed("no activity".into()))
    }
}

fn recorder_factory() -> Factory {
    Arc::new(|id, input| {
        Ok(match input {
            Some(_) => Box::new(Machine::new(
                Recorder,
                id,
                Heard(vec![]),
                Box::new(NoActivities),
            )),
            None => Box::new(Machine::resume_only(Recorder, id, Box::new(NoActivities))),
        })
    })
}

fn template() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/team.toml");
    std::fs::read_to_string(p)
        .expect("read template team.toml")
        .replace("harness = \"omp\"", "harness = \"claude\"")
}

/// A seeded database: project 1, a task, and the edges the cases need.
fn seed(ws: &TestWorkspace) -> Connection {
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
    for (from, to, kind) in [("lead", "peer", "spawn"), ("lead", "peer", "mail")] {
        conn.execute(
            "INSERT OR IGNORE INTO topology_edge (from_role, to_role, edge) VALUES (?1, ?2, ?3)",
            params![from, to, kind],
        )
        .unwrap();
    }
    conn
}

fn agent(conn: &Connection, name: &str, role: &str, spawner: Option<i64>) -> i64 {
    conn.execute(
        "INSERT INTO agent (name, role, project_id, spawner_id, started_ms) \
         VALUES (?1, ?2, 1, ?3, 1)",
        params![name, role, spawner],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn start(ws: &TestWorkspace, herdr: Arc<FakeHerdr>) -> DaemonHandle {
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone(),
    });
    d.register_kind("recorder", recorder_factory()).unwrap();
    bus::install(&mut CliRegistry::new(), &mut d).expect("bus installs");
    worker_flow::register(
        &mut d,
        Deps {
            herdr,
            runs_dir: ws.runs_dir(),
            drivers: Drivers::native(),
        },
    )
    .expect("worker flow registers");
    d.spawn().expect("daemon starts")
}

fn call(ws: &TestWorkspace, caller: Option<&str>, verb: &str, args: Value) -> Response {
    let mut req = Request::new(verb, args);
    req.caller = caller.map(String::from);
    rpc::call(&rpc::socket_path(&ws.runs_dir()), &req).expect("rpc transport")
}

fn ok(ws: &TestWorkspace, caller: Option<&str>, verb: &str, args: Value) -> Value {
    call(ws, caller, verb, args)
        .into_result()
        .unwrap_or_else(|e| panic!("{verb} refused: {e}"))
}

/// The refusal code of a call that must fail.
fn refusal(ws: &TestWorkspace, caller: &str, verb: &str, args: Value) -> String {
    let r = call(ws, Some(caller), verb, args);
    assert!(!r.ok, "{verb} must be refused");
    r.error.unwrap().code
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The `source/name` of every Signal journaled for `id`, in order.
fn heard(ws: &TestWorkspace, id: &str) -> Vec<String> {
    SqliteJournal::open(&ws.db_path())
        .unwrap()
        .load_events(id, 1)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "signal")
        .filter_map(|e| {
            let v: Value = serde_json::from_str(&e.payload_json).ok()?;
            Some(format!("{}/{}", v["source"].as_str()?, v["name"].as_str()?))
        })
        .collect()
}

fn count(ws: &TestWorkspace, id: &str, what: &str) -> usize {
    heard(ws, id).iter().filter(|h| *h == what).count()
}

fn refusals(ws: &TestWorkspace) -> Vec<(String, String, String)> {
    let conn = db::open(&ws.db_path()).unwrap();
    authority::refusals(&conn)
        .unwrap()
        .into_iter()
        .map(|r| (r.caller, r.edge, r.target))
        .collect()
}

fn task_args(name: &str) -> Value {
    json!({
        "name": name, "kind": "ni", "role": "peer", "pane": "p1",
        "prompt": "do the thing", "report": format!("{name}.md"),
        "timeout_ms": 600_000, "base": "main", "project_id": 1, "task_id": 1,
    })
}

#[test]
fn recover_rearms_watch() {
    let ws = TestWorkspace::new("seam-rearm");
    let first = Arc::new(FakeHerdr::new());
    first
        .agent_start("a1", "claude", "pane-1", 1000, &[])
        .unwrap();
    first.set_status("a1", "idle");
    let daemon = start(&ws, first);
    ok(
        &ws,
        None,
        "workflow.start",
        json!({"kind": "recorder", "id": "wf", "input": {}}),
    );
    ok(
        &ws,
        None,
        "watch.agent",
        json!({"workflow": "wf", "agent": "a1"}),
    );
    wait_for("first status signal", || {
        count(&ws, "wf", "herdr/status") == 1
    });
    daemon.stop().unwrap();

    // A new daemon, a new herdr: the agent changed while nobody watched.
    let second = Arc::new(FakeHerdr::new());
    second
        .agent_start("a1", "claude", "pane-1", 1000, &[])
        .unwrap();
    second.set_status("a1", "working");
    let daemon = start(&ws, second.clone());
    wait_for("status signal after restart", || {
        count(&ws, "wf", "herdr/status") == 2
    });
    // The re-armed watch keeps following the agent, not just the first read.
    second.set_status("a1", "done");
    wait_for("status change after restart", || {
        count(&ws, "wf", "herdr/status") == 3
    });
    daemon.stop().unwrap();
}

#[test]
fn late_workflow_gets_mail_signal() {
    let ws = TestWorkspace::new("seam-late");
    let conn = seed(&ws);
    let lead = agent(&conn, "lead-a", "lead", None);
    agent(&conn, "peer-a", "peer", Some(lead));
    drop(conn);
    let daemon = start(&ws, Arc::new(FakeHerdr::new()));
    let sent = ok(
        &ws,
        Some("lead-a"),
        "mail.send",
        json!({"to": "peer-a", "body": "hello"}),
    );
    assert_eq!(sent["signalled"], false, "no workflow yet");

    let started = json!({"kind": "recorder", "id": "peer-a", "input": {}});
    ok(&ws, None, "workflow.start", started.clone());
    wait_for("one mail signal", || count(&ws, "peer-a", "mail/mail") == 1);
    // A repeat start is a no-op: no second Signal.
    assert_eq!(ok(&ws, None, "workflow.start", started)["created"], false);
    ok(
        &ws,
        None,
        "signal",
        json!({"workflow": "peer-a", "name": "barrier"}),
    );
    wait_for("barrier", || count(&ws, "peer-a", "rpc/barrier") == 1);
    assert_eq!(count(&ws, "peer-a", "mail/mail"), 1);

    // Unacked mail is signalled again once after a restart; acked mail is not.
    daemon.stop().unwrap();
    let daemon = start(&ws, Arc::new(FakeHerdr::new()));
    wait_for("signal after restart", || {
        count(&ws, "peer-a", "mail/mail") == 2
    });
    ok(
        &ws,
        Some("peer-a"),
        "mail.ack",
        json!({"upto": sent["id"].as_i64().unwrap()}),
    );
    daemon.stop().unwrap();
    let daemon = start(&ws, Arc::new(FakeHerdr::new()));
    ok(
        &ws,
        None,
        "signal",
        json!({"workflow": "peer-a", "name": "barrier2"}),
    );
    wait_for("barrier2", || count(&ws, "peer-a", "rpc/barrier2") == 1);
    assert_eq!(count(&ws, "peer-a", "mail/mail"), 2);
    daemon.stop().unwrap();
}

#[test]
fn mail_send_refused_off_edge() {
    let ws = TestWorkspace::new("seam-mail");
    let conn = seed(&ws);
    let lead = agent(&conn, "lead-a", "lead", None);
    agent(&conn, "peer-a", "peer", Some(lead));
    agent(&conn, "peer-b", "peer", Some(lead));
    drop(conn);
    let daemon = start(&ws, Arc::new(FakeHerdr::new()));

    ok(
        &ws,
        Some("lead-a"),
        "mail.send",
        json!({"to": "peer-a", "body": "on the edge"}),
    );
    let code = refusal(
        &ws,
        "peer-a",
        "mail.send",
        json!({"to": "peer-b", "body": "peer to peer"}),
    );
    assert_eq!(code, "refused");
    assert_eq!(
        refusals(&ws),
        vec![("peer-a".into(), "mail".into(), "peer-b".into())]
    );
    let inbox = ok(&ws, Some("peer-b"), "mail.inbox", Value::Null);
    assert_eq!(inbox["mail"].as_array().unwrap().len(), 0, "nothing stored");
    // An unknown sender is refused too, and journaled.
    let code = refusal(
        &ws,
        "ghost",
        "mail.send",
        json!({"to": "peer-a", "body": "who"}),
    );
    assert_eq!(code, "refused");
    assert_eq!(refusals(&ws).len(), 2);
    daemon.stop().unwrap();
}

#[test]
fn task_start_refused_off_edge() {
    let ws = TestWorkspace::new("seam-spawn");
    let conn = seed(&ws);
    let lead = agent(&conn, "lead-a", "lead", None);
    agent(&conn, "peer-a", "peer", Some(lead));
    drop(conn);
    let herdr = Arc::new(FakeHerdr::new());
    let daemon = start(&ws, herdr.clone());

    // A Peer spawns nothing: refused, journaled, and no agent appears.
    let code = refusal(&ws, "peer-a", "task.start", task_args("w-denied"));
    assert_eq!(code, "refused");
    assert_eq!(
        refusals(&ws),
        vec![("peer-a".into(), "spawn".into(), "peer".into())]
    );
    let conn = db::open(&ws.db_path()).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM agent WHERE name = 'w-denied'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "a refused spawn creates no agent");
    assert!(
        herdr.start_args("w-denied").is_empty(),
        "herdr never started it"
    );

    // The Lead's edge to Peers is open.
    ok(&ws, Some("lead-a"), "task.start", task_args("w-ok"));
    daemon.stop().unwrap();
}

#[test]
fn spawner_id_recorded() {
    let ws = TestWorkspace::new("seam-spawner");
    let conn = seed(&ws);
    let lead = agent(&conn, "lead-a", "lead", None);
    drop(conn);
    let daemon = start(&ws, Arc::new(FakeHerdr::new()));

    let args = task_args("w-1");
    let first = ok(&ws, Some("lead-a"), "task.start", args.clone());
    // A retry converges on the same agent and is not re-gated.
    let again = ok(&ws, Some("lead-a"), "task.start", args);
    assert_eq!(first["agent_id"], again["agent_id"]);
    let conn = db::open(&ws.db_path()).unwrap();
    let spawner: Option<i64> = conn
        .query_row("SELECT spawner_id FROM agent WHERE name = 'w-1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(spawner, Some(lead));
    assert!(refusals(&ws).is_empty());
    daemon.stop().unwrap();
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn publish_report(ws: &TestWorkspace, name: &str) {
    let tmp = ws.runs_dir().join(format!(".{name}.tmp"));
    std::fs::write(
        &tmp,
        format!("Outcome: COMPLETE\nVerified: unit-test-verified {SHA}\n\n## Blockers\nnone\n"),
    )
    .unwrap();
    std::fs::rename(tmp, ws.runs_dir().join(format!("{name}.md"))).unwrap();
}

fn flow_state(ws: &TestWorkspace, id: &str) -> String {
    let conn = db::open(&ws.db_path()).unwrap();
    conn.query_row("SELECT state FROM workflow WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .unwrap()
}

/// Starts a worker flow under `lead-a` and waits until its prompt is out.
fn start_worker(ws: &TestWorkspace, name: &str) -> (DaemonHandle, Arc<FakeHerdr>) {
    let conn = seed(ws);
    let lead = agent(&conn, "lead-a", "lead", None);
    agent(&conn, "peer-a", "peer", Some(lead));
    drop(conn);
    let herdr = Arc::new(FakeHerdr::new());
    let daemon = start(ws, herdr.clone());
    ok(ws, Some("lead-a"), "task.start", task_args(name));
    wait_for("the prompt", || !herdr.prompts().is_empty());
    (daemon, herdr)
}

fn restarted(ws: &TestWorkspace) -> DaemonHandle {
    start(ws, Arc::new(FakeHerdr::new()))
}

#[test]
fn report_after_restart_reaches_flow() {
    let ws = TestWorkspace::new("seam-report-after");
    let (daemon, _) = start_worker(&ws, "w-after");
    daemon.stop().unwrap();
    let daemon = restarted(&ws);
    assert_eq!(flow_state(&ws, "worker:w-after"), "running");
    publish_report(&ws, "w-after");
    wait_for("report signal after restart", || {
        count(&ws, "worker:w-after", "report/appeared") == 1
    });
    wait_for("flow to settle", || {
        flow_state(&ws, "worker:w-after") != "running"
    });
    daemon.stop().unwrap();
}

#[test]
fn report_written_while_down_is_seen_on_recover() {
    let ws = TestWorkspace::new("seam-report-down");
    let (daemon, _) = start_worker(&ws, "w-down");
    daemon.stop().unwrap();
    publish_report(&ws, "w-down");
    let daemon = restarted(&ws);
    wait_for("report signal on recover", || {
        count(&ws, "worker:w-down", "report/appeared") == 1
    });
    wait_for("flow to settle", || {
        flow_state(&ws, "worker:w-down") != "running"
    });
    daemon.stop().unwrap();
}

#[test]
fn task_start_on_open_name_gated_unless_own_spawner() {
    let ws = TestWorkspace::new("seam-open-gate");
    let (daemon, _) = start_worker(&ws, "w-open");
    // A Peer cannot take over an open agent's name: refused and journaled.
    let code = refusal(&ws, "peer-a", "task.start", task_args("w-open"));
    assert_eq!(code, "refused");
    assert_eq!(
        refusals(&ws),
        vec![("peer-a".into(), "spawn".into(), "peer".into())]
    );
    // The recorded spawner's retry converges, ungated.
    ok(&ws, Some("lead-a"), "task.start", task_args("w-open"));
    assert_eq!(refusals(&ws).len(), 1);
    daemon.stop().unwrap();
}

#[test]
fn mail_reaches_worker_flow() {
    let ws = TestWorkspace::new("seam-worker-mail");
    let (daemon, _) = start_worker(&ws, "w-mail");
    let sent = ok(
        &ws,
        Some("lead-a"),
        "mail.send",
        json!({"to": "w-mail", "body": "hello"}),
    );
    assert_eq!(sent["signalled"], true);
    wait_for("push to worker flow", || {
        count(&ws, "worker:w-mail", "mail/mail") == 1
    });
    // Unacked mail is signalled again to the recovered worker flow.
    daemon.stop().unwrap();
    let daemon = restarted(&ws);
    wait_for("signal after restart", || {
        count(&ws, "worker:w-mail", "mail/mail") == 2
    });
    daemon.stop().unwrap();
}
