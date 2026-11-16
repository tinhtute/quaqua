//! Mail through the daemon's RPC door: delivered once, journaled, signalled.

use std::sync::Arc;
use std::time::{Duration, Instant};

use qq_core::bus;
use qq_core::cli::Registry;
use qq_core::daemon::supervisor::{Factory, Machine};
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::engine::journal::Journal;
use qq_core::engine::journal_sqlite::SqliteJournal;
use qq_core::engine::{
    Activity, ActivityError, ActivityRequest, ActivityResult, StepDecision, Workflow,
    WorkflowContext, WorkflowEvent,
};
use qq_core::herdr::FakeHerdr;
use qq_core::rpc::{self, Request, Response};
use qq_core::testkit::TestWorkspace;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Counts the Signals it receives; ends on `done`.
struct Listener;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Heard(Vec<Value>);

impl Workflow for Listener {
    type Input = Value;
    type State = Heard;
    type Output = usize;

    fn step(
        &self,
        state: &mut Heard,
        _: &mut WorkflowContext,
        event: Option<WorkflowEvent>,
    ) -> StepDecision<usize> {
        match event {
            Some(WorkflowEvent::Signal { name, payload, .. }) if name == "mail" => {
                state.0.push(payload);
                StepDecision::Continue
            }
            Some(WorkflowEvent::Signal { .. }) => StepDecision::Complete(state.0.len()),
            _ => StepDecision::Continue,
        }
    }
}

struct NoActivities;

impl Activity for NoActivities {
    fn execute(&self, _: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        Err(ActivityError::ExecutionFailed("no activity".into()))
    }
}

fn listener_factory() -> Factory {
    Arc::new(|id, input| {
        Ok(match input {
            Some(_) => Box::new(Machine::new(
                Listener,
                id,
                Heard(vec![]),
                Box::new(NoActivities),
            )),
            None => Box::new(Machine::resume_only(Listener, id, Box::new(NoActivities))),
        })
    })
}

fn start(ws: &TestWorkspace) -> DaemonHandle {
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: Arc::new(FakeHerdr::new()),
    });
    d.register_kind("listener", listener_factory()).unwrap();
    bus::install(&mut Registry::new(), &mut d).expect("bus installs");
    d.spawn().expect("daemon starts")
}

fn call(ws: &TestWorkspace, caller: &str, verb: &str, args: Value) -> Response {
    let mut req = Request::new(verb, args);
    req.caller = Some(caller.into());
    rpc::call(&rpc::socket_path(&ws.runs_dir()), &req).expect("rpc transport")
}

fn ok(ws: &TestWorkspace, caller: &str, verb: &str, args: Value) -> Value {
    call(ws, caller, verb, args)
        .into_result()
        .unwrap_or_else(|e| panic!("{verb} refused: {e}"))
}

fn events(ws: &TestWorkspace, id: &str) -> Vec<(String, Value)> {
    SqliteJournal::open(&ws.db_path())
        .unwrap()
        .load_events(id, 1)
        .unwrap()
        .into_iter()
        .map(|e| {
            (
                e.kind,
                serde_json::from_str(&e.payload_json).unwrap_or(Value::Null),
            )
        })
        .collect()
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn ids(inbox: &Value) -> Vec<i64> {
    inbox["mail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect()
}

#[test]
fn mail_delivered_once_and_journaled() {
    let ws = TestWorkspace::new("mail-once");
    let daemon = start(&ws);
    ok(
        &ws,
        "lead",
        "workflow.start",
        json!({"kind": "listener", "id": "peer-a", "input": {}}),
    );

    let sent = ok(
        &ws,
        "lead",
        "mail.send",
        json!({"to": "peer-a", "body": "build T38", "kind": "task", "dedupe_key": "k1"}),
    );
    assert_eq!(sent["created"], true);
    let id = sent["id"].as_i64().unwrap();

    // A client retry with the same key is the same mail, not a second one.
    let retry = ok(
        &ws,
        "lead",
        "mail.send",
        json!({"to": "peer-a", "body": "build T38", "kind": "task", "dedupe_key": "k1"}),
    );
    assert_eq!(
        (retry["id"].as_i64(), retry["created"].clone()),
        (Some(id), json!(false))
    );

    // Reaches the inbox once; the read is the delivery, a re-read is empty.
    let first = ok(&ws, "peer-a", "mail.inbox", Value::Null);
    assert_eq!(ids(&first), vec![id]);
    assert_eq!(first["mail"][0]["from"], "lead");
    assert_eq!(first["mail"][0]["body"], "build T38");
    assert_eq!(first["mail"][0]["kind"], "task");
    assert!(ids(&ok(&ws, "peer-a", "mail.inbox", Value::Null)).is_empty());

    // Journaled exactly once, with sender and addressee.
    let j = events(&ws, &format!("mail:{id}"));
    assert_eq!(j.len(), 1);
    assert_eq!(j[0].0, "mail_sent");
    assert_eq!(
        (j[0].1["from"].clone(), j[0].1["to"].clone()),
        (json!("lead"), json!("peer-a"))
    );

    // One push to the addressee's workflow, none for the retry.
    wait_for("the mail signal", || {
        events(&ws, "peer-a")
            .iter()
            .filter(|(k, _)| k == "signal")
            .count()
            == 1
    });
    ok(
        &ws,
        "lead",
        "signal",
        json!({"workflow": "peer-a", "name": "done"}),
    );
    wait_for("listener done", || {
        events(&ws, "peer-a")
            .iter()
            .any(|(k, _)| k == "workflow_completed")
    });
    let signals = events(&ws, "peer-a")
        .iter()
        .filter(|(k, _)| k == "signal")
        .count();
    assert_eq!(signals, 2, "one mail signal plus the done signal");

    daemon.stop().unwrap();
}

#[test]
fn inbox_cursors_are_per_addressee_and_survive_restart() {
    let ws = TestWorkspace::new("mail-cursor");
    let daemon = start(&ws);

    // No workflow named like the addressee: still stored, no error.
    let a = ok(
        &ws,
        "lead",
        "mail.send",
        json!({"to": "peer-a", "body": "1"}),
    );
    assert_eq!(a["signalled"], false);
    let b = ok(
        &ws,
        "lead",
        "mail.send",
        json!({"to": "peer-b", "body": "for b"}),
    );
    let a2 = ok(
        &ws,
        "lead",
        "mail.send",
        json!({"to": "peer-a", "body": "2"}),
    );
    let (a, b, a2) = (
        a["id"].as_i64().unwrap(),
        b["id"].as_i64().unwrap(),
        a2["id"].as_i64().unwrap(),
    );

    // Peek leaves the cursor; ack moves it, never backwards, never past the newest.
    assert_eq!(
        ids(&ok(&ws, "peer-a", "mail.inbox", json!({"peek": true}))),
        vec![a, a2]
    );
    assert_eq!(
        ids(&ok(&ws, "peer-a", "mail.inbox", json!({"peek": true}))),
        vec![a, a2]
    );
    ok(&ws, "peer-a", "mail.ack", json!({"upto": a}));
    ok(&ws, "peer-a", "mail.ack", json!({"upto": 0}));
    assert_eq!(
        ids(&ok(&ws, "peer-a", "mail.inbox", json!({"peek": true}))),
        vec![a2]
    );

    // A wild ack cannot swallow mail that has not arrived yet.
    ok(&ws, "peer-a", "mail.ack", json!({"upto": 9999}));
    let late = ok(
        &ws,
        "lead",
        "mail.send",
        json!({"to": "peer-a", "body": "3"}),
    );
    assert_eq!(
        ids(&ok(&ws, "peer-a", "mail.inbox", Value::Null)),
        vec![late["id"].as_i64().unwrap()]
    );

    // peer-b's box is untouched by peer-a's reads, and survives a restart.
    daemon.stop().unwrap();
    let daemon = start(&ws);
    assert_eq!(ids(&ok(&ws, "peer-b", "mail.inbox", Value::Null)), vec![b]);
    assert!(ids(&ok(&ws, "peer-b", "mail.inbox", Value::Null)).is_empty());
    assert!(ids(&ok(&ws, "peer-a", "mail.inbox", Value::Null)).is_empty());

    // No sender, no mail.
    let r = rpc::call(
        &rpc::socket_path(&ws.runs_dir()),
        &Request::new("mail.send", json!({"to": "x", "body": "y"})),
    )
    .unwrap();
    assert_eq!(r.error.unwrap().code, rpc::BAD_REQUEST);
    daemon.stop().unwrap();
}
