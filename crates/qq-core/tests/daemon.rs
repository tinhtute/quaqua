//! Daemon core, through its two doors: the Unix-socket RPC and the `quaqua` CLI.
//! Workflows are driven only by pushed Signals and real deadlines.

use std::collections::BTreeSet;
use std::io;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use qq_core::daemon::store::Store;
use qq_core::daemon::supervisor::Supervisor;
use qq_core::daemon::supervisor::{Factory, Machine};
use qq_core::daemon::{Config, Daemon, DaemonError, DaemonHandle};
use qq_core::engine::journal::Journal;
use qq_core::engine::journal_sqlite::SqliteJournal;
use qq_core::engine::{
    Activity, ActivityError, ActivityRequest, ActivityResult, StepDecision, Workflow,
    WorkflowContext, WorkflowEvent,
};
use qq_core::herdr::{FakeHerdr, HerdrClient, Reply};
use qq_core::rpc::{self, Request};
use qq_core::testkit::TestWorkspace;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CollectState {
    idle_ms: u64,
    seen: Vec<String>,
}

/// Arms one idle timer, folds every Signal name into state, ends on `done`
/// or when the timer fires.
struct Collector;

impl Workflow for Collector {
    type Input = Value;
    type State = CollectState;
    type Output = Vec<String>;

    fn step(
        &self,
        state: &mut CollectState,
        _ctx: &mut WorkflowContext,
        event: Option<WorkflowEvent>,
    ) -> StepDecision<Vec<String>> {
        match event {
            None => StepDecision::ScheduleTimer {
                id: "idle".into(),
                duration: Duration::from_millis(state.idle_ms),
            },
            Some(WorkflowEvent::Signal { name, .. }) if name == "done" => {
                StepDecision::Complete(state.seen.clone())
            }
            Some(WorkflowEvent::Signal { name, .. }) => {
                state.seen.push(name);
                StepDecision::Continue
            }
            Some(WorkflowEvent::TimerFired { .. }) => StepDecision::Complete(state.seen.clone()),
            Some(other) => StepDecision::Fail {
                code: 1,
                reason: format!("unexpected {other:?}"),
            },
        }
    }
}

struct NoActivities;

impl Activity for NoActivities {
    fn execute(&self, _: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        Err(ActivityError::ExecutionFailed("no activity may run".into()))
    }
}

fn collector_factory() -> Factory {
    Arc::new(|id, input| {
        Ok(match input {
            Some(v) => Box::new(Machine::new(
                Collector,
                id,
                CollectState {
                    idle_ms: v["idle_ms"].as_u64().unwrap_or(3_600_000),
                    seen: vec![],
                },
                Box::new(NoActivities),
            )),
            None => Box::new(Machine::resume_only(Collector, id, Box::new(NoActivities))),
        })
    })
}

fn start_daemon(
    ws: &TestWorkspace,
    herdr: Arc<dyn HerdrClient>,
) -> Result<DaemonHandle, DaemonError> {
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr,
    });
    d.register_kind("collector", collector_factory()).unwrap();
    d.register_verb(
        "whoami",
        Box::new(|_, req| Ok(json!({ "caller": req.caller }))),
    )
    .unwrap();
    d.spawn()
}

fn rpc_ok(ws: &TestWorkspace, verb: &str, args: Value) -> Value {
    rpc::call(&rpc::socket_path(&ws.runs_dir()), &Request::new(verb, args))
        .expect("rpc transport")
        .into_result()
        .unwrap_or_else(|e| panic!("{verb} refused: {e}"))
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn workflow_state(ws: &TestWorkspace, id: &str) -> Option<String> {
    let st = rpc_ok(ws, "status", Value::Null);
    st["workflows"]
        .as_array()?
        .iter()
        .find(|w| w["id"] == id)
        .map(|w| w["state"].as_str().unwrap_or("").to_string())
}

fn journal_rows(ws: &TestWorkspace, id: &str) -> Vec<(String, Value)> {
    let j = SqliteJournal::open(&ws.db_path()).expect("open journal");
    j.load_events(id, 1)
        .expect("load events")
        .into_iter()
        .map(|e| {
            let p = serde_json::from_str(&e.payload_json).unwrap_or(Value::Null);
            (e.kind, p)
        })
        .collect()
}

fn count(rows: &[(String, Value)], kind: &str) -> usize {
    rows.iter().filter(|(k, _)| k == kind).count()
}

fn qq(runs: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .args(args)
        .env("QUAQUA_RUNS_DIR", runs)
        .env("QUAQUA_TEST", "1")
        .output()
        .expect("run qq")
}

#[test]
fn version_flags_print_version_and_exit_0() {
    let ws = TestWorkspace::new("version");
    for flag in ["--version", "-V"] {
        let out = qq(&ws.runs_dir(), &[flag]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            concat!("quaqua ", env!("CARGO_PKG_VERSION")),
            "{flag}"
        );
    }
}

#[test]
fn daemon_rpc_roundtrip() {
    let ws = TestWorkspace::new("rpc");
    let daemon = start_daemon(&ws, Arc::new(FakeHerdr::new())).expect("daemon starts");

    // A result comes back at once.
    let pong = rpc_ok(&ws, "ping", Value::Null);
    assert_eq!(pong["pong"], true);

    // Immediate refusals, each with a code callers branch on.
    let sock = rpc::socket_path(&ws.runs_dir());
    let unknown = rpc::call(&sock, &Request::new("no.such.verb", Value::Null)).unwrap();
    assert!(!unknown.ok);
    assert_eq!(unknown.error.unwrap().code, rpc::UNKNOWN_VERB);
    let missing = rpc::call(&sock, &Request::new("signal", json!({"name": "x"}))).unwrap();
    assert_eq!(missing.error.unwrap().code, rpc::BAD_REQUEST);
    let nowf = rpc::call(
        &sock,
        &Request::new("signal", json!({"workflow": "ghost", "name": "x"})),
    )
    .unwrap();
    assert_eq!(nowf.error.unwrap().code, "no_such_workflow");

    // Handlers see the caller of the request.
    let who = rpc::call(
        &sock,
        &Request {
            verb: "whoami".into(),
            args: Value::Null,
            caller: Some("peer-a".into()),
            caller_pane: None,
        },
    )
    .unwrap()
    .into_result()
    .unwrap();
    assert_eq!(who["caller"], "peer-a");

    // One connection, several lines: one response line each, in order.
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    s.write_all(b"not json\n{\"verb\":\"ping\"}\n").unwrap();
    let mut r = std::io::BufReader::new(s);
    let first: rpc::Response =
        serde_json::from_str(&rpc::read_line(&mut r).unwrap().unwrap()).unwrap();
    assert_eq!(first.error.unwrap().code, rpc::BAD_REQUEST);
    let second: rpc::Response =
        serde_json::from_str(&rpc::read_line(&mut r).unwrap().unwrap()).unwrap();
    assert!(second.ok);

    // The CLI reaches the same daemon: a result, then a refusal, then no daemon.
    let out = qq(&ws.runs_dir(), &["ping"]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("pong"));
    let refused = qq(&ws.runs_dir(), &["call", "no.such.verb"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr).contains(rpc::UNKNOWN_VERB));
    assert_eq!(qq(&ws.runs_dir(), &["bogus"]).status.code(), Some(2));

    // A second daemon on the same workspace is refused.
    let second = start_daemon(&ws, Arc::new(FakeHerdr::new()));
    assert!(matches!(second, Err(DaemonError::AlreadyRunning(_))));

    daemon.stop().expect("clean stop");
    assert!(!sock.exists(), "socket removed on stop");
    let gone = qq(&ws.runs_dir(), &["ping"]);
    assert_eq!(gone.status.code(), Some(69), "daemon unreachable has its own code");
    assert!(String::from_utf8_lossy(&gone.stderr).contains("daemon not running"));
}

#[test]
fn daemon_start_replaces_stale_socket() {
    let ws = TestWorkspace::new("stale");
    let sock = rpc::socket_path(&ws.runs_dir());
    // A crashed daemon leaves its socket file behind with nobody listening.
    drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
    assert!(sock.exists());
    let daemon = start_daemon(&ws, Arc::new(FakeHerdr::new())).expect("replaces stale socket");
    assert_eq!(rpc_ok(&ws, "ping", Value::Null)["pong"], true);
    daemon.stop().unwrap();
}

#[test]
fn state_single_home() {
    let ws = TestWorkspace::new("home");
    let daemon = start_daemon(&ws, Arc::new(FakeHerdr::new())).unwrap();
    let started = rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "wf1"}),
    );
    assert_eq!(started["created"], true);
    // Starting the same id again changes nothing.
    let again = rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "wf1"}),
    );
    assert_eq!(again["created"], false);
    rpc_ok(&ws, "signal", json!({"workflow": "wf1", "name": "a"}));
    rpc_ok(&ws, "signal", json!({"workflow": "wf1", "name": "b"}));
    wait_for("both signals journaled", || {
        count(&journal_rows(&ws, "wf1"), "signal") == 2
    });
    daemon.stop().unwrap();

    // Stopped: the workspace holds the database and nothing else.
    let files: BTreeSet<String> = std::fs::read_dir(ws.runs_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let allowed: BTreeSet<String> = ["qq.db", "qq.db-wal", "qq.db-shm"].map(String::from).into();
    assert!(files.is_subset(&allowed), "sidecar state files: {files:?}");
    assert!(files.contains("qq.db"));

    // A new daemon knows the workflow from the database alone and continues it.
    let daemon = start_daemon(&ws, Arc::new(FakeHerdr::new())).unwrap();
    let st = rpc_ok(&ws, "status", Value::Null);
    let wf = &st["workflows"][0];
    assert_eq!(wf["id"], "wf1");
    assert_eq!(wf["state"], "running");
    assert_eq!(wf["loop"], true, "loop revived from the database");
    rpc_ok(&ws, "signal", json!({"workflow": "wf1", "name": "done"}));
    wait_for("workflow completes", || {
        workflow_state(&ws, "wf1").as_deref() == Some("completed")
    });
    let rows = journal_rows(&ws, "wf1");
    assert_eq!(count(&rows, "signal"), 3);
    assert_eq!(count(&rows, "workflow_completed"), 1);
    // Completed means finished: starting it again is a no-op, not a rerun.
    let rerun = rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "wf1"}),
    );
    assert_eq!(rerun["created"], false);
    daemon.stop().unwrap();
}

#[test]
fn daemon_loop_fires_deadline_and_idles_quietly() {
    let ws = TestWorkspace::new("deadline");
    let daemon = start_daemon(&ws, Arc::new(FakeHerdr::new())).unwrap();
    rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "idler", "input": {"idle_ms": 300}}),
    );
    rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "quiet"}),
    );
    // The deadline alone ends `idler`: no event is pushed.
    wait_for("idle timer fires", || {
        workflow_state(&ws, "idler").as_deref() == Some("completed")
    });
    let idler = journal_rows(&ws, "idler");
    assert_eq!(count(&idler, "timer_fired"), 1);
    assert_eq!(count(&idler, "signal"), 0);
    // `quiet` waits on a one-hour timer: while it idles nothing is journaled.
    let before = journal_rows(&ws, "quiet").len();
    std::thread::sleep(Duration::from_millis(500));
    let after = journal_rows(&ws, "quiet");
    assert_eq!(after.len(), before, "idle workflow wrote rows: {after:?}");
    assert!(after.iter().all(|(k, p)| {
        !k.starts_with("activity_") && !p.to_string().contains("wake_") && k != "signal"
    }));
    daemon.stop().unwrap();
}

#[test]
fn watchers_emit_signals_on_change_only() {
    let ws = TestWorkspace::new("watch");
    let fake = Arc::new(FakeHerdr::new());
    fake.agent_start("w1", "claude", "pane-1", 1000, &[])
        .unwrap();
    let daemon = start_daemon(&ws, fake.clone()).unwrap();
    rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "wf"}),
    );
    let report = ws.runs_dir().join("w1.md");
    rpc_ok(
        &ws,
        "watch.report",
        json!({"workflow": "wf", "path": report}),
    );

    fake.script_status("w1", &["working", "blocked"]);
    rpc_ok(&ws, "watch.agent", json!({"workflow": "wf", "agent": "w1"}));
    // A repeat of the same watch must not journal each change twice.
    rpc_ok(&ws, "watch.agent", json!({"workflow": "wf", "agent": "w1"}));
    rpc_ok(
        &ws,
        "watch.report",
        json!({"workflow": "wf", "path": report}),
    );
    let statuses = |rows: &[(String, Value)]| -> Vec<String> {
        rows.iter()
            .filter(|(k, p)| k == "signal" && p.to_string().contains("\"herdr\""))
            .map(|(_, p)| p.to_string())
            .collect()
    };
    wait_for("idle, working, blocked", || {
        statuses(&journal_rows(&ws, "wf")).len() == 3
    });
    // Stable status: the watcher keeps waiting and journals nothing more.
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(statuses(&journal_rows(&ws, "wf")).len(), 3);
    fake.set_status("w1", "done");
    wait_for("done", || statuses(&journal_rows(&ws, "wf")).len() == 4);
    let seen = statuses(&journal_rows(&ws, "wf"));
    for (got, want) in seen.iter().zip(["idle", "working", "blocked", "done"]) {
        assert!(
            got.contains(&format!("\"status\":\"{want}\"")),
            "{got} vs {want}"
        );
    }

    // The report appears (atomic rename): exactly one Signal, nothing before it.
    let reports = |rows: &[(String, Value)]| {
        rows.iter()
            .filter(|(k, p)| k == "signal" && p.to_string().contains("appeared"))
            .count()
    };
    assert_eq!(reports(&journal_rows(&ws, "wf")), 0);
    let tmp = ws.runs_dir().join(".w1.tmp");
    std::fs::write(&tmp, "Outcome: COMPLETE\n").unwrap();
    std::fs::rename(&tmp, &report).unwrap();
    wait_for("report signal", || reports(&journal_rows(&ws, "wf")) == 1);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(reports(&journal_rows(&ws, "wf")), 1);
    daemon.stop().unwrap();
}

#[test]
fn acknowledged_signal_survives_immediate_stop() {
    let ws = TestWorkspace::new("flush");
    let daemon = start_daemon(&ws, Arc::new(FakeHerdr::new())).unwrap();
    rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "wf"}),
    );
    for n in ["a", "b", "c"] {
        let r = rpc_ok(&ws, "signal", json!({"workflow": "wf", "name": n}));
        assert_eq!(r["delivered"], true);
    }
    // No wait: stop right behind the acknowledgements.
    daemon.stop().unwrap();
    assert_eq!(count(&journal_rows(&ws, "wf"), "signal"), 3);
}

/// `agent get` answers `working`; `agent wait` blocks until released, as the
/// real binary does (herdr cannot cancel a wait).
struct BlockingHerdr {
    waiting: AtomicBool,
    release: AtomicBool,
}

impl BlockingHerdr {
    fn new() -> Self {
        BlockingHerdr {
            waiting: AtomicBool::new(false),
            release: AtomicBool::new(false),
        }
    }
}

impl HerdrClient for BlockingHerdr {
    fn agent_start(&self, _: &str, _: &str, _: &str, _: u32, _: &[&str]) -> io::Result<Reply> {
        Ok(Reply::err("unsupported"))
    }
    fn agent_get(&self, _: &str) -> io::Result<Reply> {
        Ok(Reply::ok(
            r#"{"result":{"agent":{"agent_status":"working"}}}"#,
        ))
    }
    fn agent_list(&self) -> io::Result<Reply> {
        Ok(Reply::err("unsupported"))
    }
    fn agent_wait(&self, _: &str, _: &[&str], _: u64) -> io::Result<Reply> {
        self.waiting.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.release.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(Reply::err("timeout"))
    }
    fn agent_prompt(&self, _: &str, _: &str, _: Option<(&[&str], u64)>) -> io::Result<Reply> {
        Ok(Reply::err("unsupported"))
    }
}

/// A [`FakeHerdr`] that counts the status reads a watcher makes.
struct CountingHerdr {
    inner: FakeHerdr,
    reads: AtomicUsize,
}

impl HerdrClient for CountingHerdr {
    fn agent_start(
        &self,
        name: &str,
        kind: &str,
        pane: &str,
        timeout_ms: u32,
        args: &[&str],
    ) -> io::Result<Reply> {
        self.inner.agent_start(name, kind, pane, timeout_ms, args)
    }
    fn agent_get(&self, name: &str) -> io::Result<Reply> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.agent_get(name)
    }
    fn agent_list(&self) -> io::Result<Reply> {
        self.inner.agent_list()
    }
    fn agent_wait(&self, name: &str, until: &[&str], timeout_ms: u64) -> io::Result<Reply> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.agent_wait(name, until, timeout_ms)
    }
    fn agent_prompt(
        &self,
        name: &str,
        text: &str,
        wait: Option<(&[&str], u64)>,
    ) -> io::Result<Reply> {
        self.inner.agent_prompt(name, text, wait)
    }
}

#[test]
fn stop_frees_the_socket_while_a_watcher_is_blocked() {
    let ws = TestWorkspace::new("stopblock");
    let herdr = Arc::new(BlockingHerdr::new());
    let old = start_daemon(&ws, herdr.clone()).unwrap();
    rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "wf"}),
    );
    rpc_ok(&ws, "watch.agent", json!({"workflow": "wf", "agent": "a"}));
    wait_for("watcher blocks in agent wait", || {
        herdr.waiting.load(Ordering::SeqCst)
    });

    // Stop through the socket; the watcher stays blocked the whole time.
    let sock = rpc::socket_path(&ws.runs_dir());
    rpc_ok(&ws, "stop", Value::Null);
    let t = Instant::now();
    wait_for("socket removed", || !sock.exists());
    let new = loop {
        match start_daemon(&ws, Arc::new(FakeHerdr::new())) {
            Ok(d) => break d,
            // Only while the loops finish their last write.
            Err(DaemonError::AlreadyRunning(_)) if t.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(e) => panic!("restart refused: {e}"),
        }
    };
    assert!(
        t.elapsed() < Duration::from_secs(3),
        "restart waited on the watcher"
    );
    assert!(
        !herdr.release.load(Ordering::SeqCst),
        "the watcher was still blocked"
    );
    assert_eq!(rpc_ok(&ws, "ping", Value::Null)["pong"], true);

    new.stop().unwrap();
    herdr.release.store(true, Ordering::SeqCst);
    old.stop().unwrap();
}

#[test]
fn missing_agent_leaves_no_live_watcher() {
    let ws = TestWorkspace::new("ghost");
    let herdr = Arc::new(CountingHerdr {
        inner: FakeHerdr::new(),
        reads: AtomicUsize::new(0),
    });
    let daemon = start_daemon(&ws, herdr.clone()).unwrap();
    rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "wf"}),
    );
    let watch = || {
        rpc_ok(
            &ws,
            "watch.agent",
            json!({"workflow": "wf", "agent": "late"}),
        )
    };
    watch();
    wait_for("first read", || herdr.reads.load(Ordering::SeqCst) >= 1);
    // agent_not_found ends the watcher: no retry loop keeps reading.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(herdr.reads.load(Ordering::SeqCst), 1);

    // The ended watcher is pruned, so the same watch starts afresh once the
    // agent exists.
    herdr
        .inner
        .agent_start("late", "claude", "pane-1", 1000, &[])
        .unwrap();
    watch();
    wait_for("status signal", || {
        count(&journal_rows(&ws, "wf"), "signal") == 1
    });
    daemon.stop().unwrap();
}

#[test]
fn watcher_ends_with_its_workflow() {
    let ws = TestWorkspace::new("wfend");
    let herdr = Arc::new(CountingHerdr {
        inner: FakeHerdr::new(),
        reads: AtomicUsize::new(0),
    });
    herdr
        .inner
        .agent_start("w", "claude", "pane-1", 1000, &[])
        .unwrap();
    let daemon = start_daemon(&ws, herdr.clone()).unwrap();
    rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "wf"}),
    );
    rpc_ok(&ws, "watch.agent", json!({"workflow": "wf", "agent": "w"}));
    wait_for("first status", || {
        count(&journal_rows(&ws, "wf"), "signal") == 1
    });
    rpc_ok(&ws, "signal", json!({"workflow": "wf", "name": "done"}));
    wait_for("workflow completes", || {
        workflow_state(&ws, "wf").as_deref() == Some("completed")
    });
    // The watcher notices on its next turn and stops reading herdr.
    std::thread::sleep(Duration::from_millis(400));
    let reads = herdr.reads.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(herdr.reads.load(Ordering::SeqCst), reads);
    daemon.stop().unwrap();
}

#[test]
fn concurrent_starts_leave_one_daemon() {
    let ws = TestWorkspace::new("race");
    for round in 0..5 {
        let gate = Barrier::new(2);
        let results: Vec<_> = std::thread::scope(|s| {
            let racers: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        gate.wait();
                        start_daemon(&ws, Arc::new(FakeHerdr::new()))
                    })
                })
                .collect();
            racers.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let (won, lost): (Vec<_>, Vec<_>) = results.into_iter().partition(Result::is_ok);
        assert_eq!(won.len(), 1, "round {round}: exactly one daemon runs");
        assert!(matches!(
            lost.into_iter().next().unwrap().err(),
            Some(DaemonError::AlreadyRunning(_))
        ));
        assert_eq!(rpc_ok(&ws, "ping", Value::Null)["pong"], true);
        won.into_iter().next().unwrap().unwrap().stop().unwrap();
    }
}

#[test]
fn started_workflow_has_its_base_before_the_ack() {
    let ws = TestWorkspace::new("base");
    let daemon = start_daemon(&ws, Arc::new(FakeHerdr::new())).unwrap();
    rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "wf"}),
    );
    // The reply is out: the replay base is already in the journal.
    let j = SqliteJournal::open(&ws.db_path()).unwrap();
    assert!(j.load_latest_snapshot("wf").unwrap().is_some());
    daemon.stop().unwrap();
}

#[test]
fn supervisor_refuses_starts_after_shutdown() {
    let ws = TestWorkspace::new("closed");
    let store = Arc::new(Store::open(&ws.db_path()).unwrap());
    let kinds = [("collector".to_string(), collector_factory())].into();
    let sup = Supervisor::new(store, kinds);
    assert!(sup.start("collector", "a", &json!({})).unwrap());
    sup.shutdown();
    let err = sup.start("collector", "b", &json!({})).unwrap_err();
    assert!(err.contains("shutting down"), "{err}");
}

#[test]
fn registrations_refuse_duplicates() {
    let ws = TestWorkspace::new("dups");
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: Arc::new(FakeHerdr::new()),
    });
    d.register_kind("collector", collector_factory()).unwrap();
    assert!(d.register_kind("collector", collector_factory()).is_err());
    assert!(d
        .register_verb("ping", Box::new(|_, _| Ok(Value::Null)))
        .is_err());
}

#[test]
fn watch_report_dedupes_on_the_normalized_path() {
    let ws = TestWorkspace::new("norm");
    let daemon = start_daemon(&ws, Arc::new(FakeHerdr::new())).unwrap();
    rpc_ok(
        &ws,
        "workflow.start",
        json!({"kind": "collector", "id": "wf"}),
    );
    let abs = ws.runs_dir().join("r.md");
    // Four spellings of one file: absolute, runs-relative, dotted, parent hop.
    for p in [
        abs.to_string_lossy().into_owned(),
        "r.md".to_string(),
        "./r.md".to_string(),
        "sub/../r.md".to_string(),
    ] {
        rpc_ok(&ws, "watch.report", json!({"workflow": "wf", "path": p}));
    }
    std::fs::write(&abs, "Outcome: COMPLETE\n").unwrap();
    wait_for("report signal", || {
        count(&journal_rows(&ws, "wf"), "signal") >= 1
    });
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(count(&journal_rows(&ws, "wf"), "signal"), 1);
    daemon.stop().unwrap();
}

#[test]
fn lock_refuses_a_second_daemon_when_the_socket_file_is_gone() {
    let ws = TestWorkspace::new("lockonly");
    let daemon = start_daemon(&ws, Arc::new(FakeHerdr::new())).unwrap();
    // The socket path no longer answers, yet the first daemon still runs.
    let sock = rpc::socket_path(&ws.runs_dir());
    let aside = ws.runs_dir().join("aside.sock");
    std::fs::rename(&sock, &aside).unwrap();
    let second = start_daemon(&ws, Arc::new(FakeHerdr::new()));
    assert!(matches!(second, Err(DaemonError::AlreadyRunning(_))));
    std::fs::rename(&aside, &sock).unwrap();
    daemon.stop().unwrap();
}

/// A child forked by another thread holds a copy of the daemon's lock until it
/// execs; the lock must still be released when the daemon stops, so the next
/// daemon starts at once (under load this made restarts fail at random).
#[test]
fn a_forked_child_does_not_keep_the_stopped_daemons_lock() {
    use std::os::unix::process::CommandExt;
    let ws = TestWorkspace::new("daemon-lock-fork");
    let daemon = start_daemon(&ws, Arc::new(FakeHerdr::new())).expect("first daemon");
    // Forked while the daemon holds the lock; sleeps in the child before exec.
    let child = std::thread::spawn(|| {
        let mut cmd = Command::new("true");
        unsafe {
            cmd.pre_exec(|| {
                std::thread::sleep(Duration::from_millis(2500));
                Ok(())
            });
        }
        cmd.status().expect("child runs")
    });
    std::thread::sleep(Duration::from_millis(300));
    daemon.stop().expect("clean stop");
    let next = start_daemon(&ws, Arc::new(FakeHerdr::new()));
    let refused = matches!(next, Err(DaemonError::AlreadyRunning(_)));
    if let Ok(d) = next {
        d.stop().expect("clean stop");
    }
    child.join().unwrap();
    assert!(!refused, "the stopped daemon's lock outlived it");
}

/// `quaqua daemon` in a fresh workspace answers on its socket, the database
/// stays open to other connections while it runs, and a second `quaqua daemon`
/// is refused. On macOS a flock on `qq.db` itself blocked SQLite's own locks,
/// so the first daemon died with "database is locked".
#[test]
fn cli_daemon_starts_answers_and_refuses_a_second() {
    /// Kills the daemon on every exit path, so a failed assert leaves no orphan
    /// and no socket file.
    struct Kill(std::process::Child, std::path::PathBuf);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
            let _ = std::fs::remove_file(&self.1);
        }
    }
    let ws = TestWorkspace::new("cli-daemon");
    let runs = ws.runs_dir();
    let sock = rpc::socket_path(&runs);
    let mut first = Kill(
        Command::new(env!("CARGO_BIN_EXE_quaqua"))
            .arg("daemon")
            .env("QUAQUA_RUNS_DIR", &runs)
            .env("QUAQUA_TEST", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn daemon"),
        sock.clone(),
    );
    // Any response is an answer: outside a herdr pane the peer gate refuses
    // the caller, which only a serving daemon can do.
    let deadline = Instant::now() + Duration::from_secs(10);
    let answer = loop {
        if let Ok(r) = rpc::call(&sock, &Request::new("ping", Value::Null)) {
            break r;
        }
        if let Some(status) = first.0.try_wait().unwrap() {
            let mut err = String::new();
            io::Read::read_to_string(&mut first.0.stderr.take().unwrap(), &mut err).unwrap();
            panic!("daemon exited {status}: {err}");
        }
        assert!(Instant::now() < deadline, "daemon never answered ping");
        std::thread::sleep(Duration::from_millis(50));
    };
    let code = answer.error.as_ref().map(|e| e.code.as_str());
    assert!(answer.ok || code == Some("unmapped_peer"), "{answer:?}");

    // Another connection reads the database while the daemon runs.
    Store::open(&ws.db_path()).expect("database opens beside the daemon");

    let second = qq(&runs, &["daemon"]);
    assert_eq!(second.status.code(), Some(1), "{second:?}");
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already running"),
        "{second:?}"
    );
    assert!(
        first.0.try_wait().unwrap().is_none(),
        "first daemon still runs"
    );
}
