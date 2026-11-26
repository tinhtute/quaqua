//! The daemon names each caller from the socket peer, never from the request.
//! The test process is the peer: its own pid (or its parent's) is what the fake
//! herdr reports for a pane, so the real SO_PEERCRED read is exercised.

use std::sync::Arc;

use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::identity::peer::{parent_pid, HerdrPeers};
use qq_core::rpc::{self, Request};
use qq_core::testkit::TestWorkspace;
use serde_json::{json, Value};

struct Rig {
    _ws: TestWorkspace,
    herdr: Arc<FakeHerdr>,
    daemon: DaemonHandle,
}

fn rig(tag: &str) -> Rig {
    rig_with(tag, false)
}

fn rig_with(tag: &str, bootstrap: bool) -> Rig {
    let ws = TestWorkspace::new(tag);
    ws.seal_herdr();
    let herdr = Arc::new(FakeHerdr::new());
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone() as Arc<dyn HerdrClient>,
    });
    d.register_verb(
        "whoami",
        Box::new(|_, req| Ok(json!({ "caller": req.caller }))),
    )
    .unwrap();
    // Stand-ins for verbs whose real handlers need more wiring: the peer gate
    // runs before any handler, so a refusal here is the real refusal.
    for v in [
        "task.start",
        "worker.stop",
        "mail.inbox.x",
        "pingx",
        "status.all",
        "config",
        "roster",
    ] {
        d.register_verb(v, Box::new(|_, _| Ok(json!({ "ran": true }))))
            .unwrap();
    }
    if bootstrap {
        d.require_peer(Arc::new(HerdrPeers::with_cos_bootstrap(
            herdr.clone(),
            ws.runs_dir(),
        )));
    } else {
        d.require_peer(Arc::new(HerdrPeers::new(herdr.clone())));
    }
    let daemon = d.spawn().unwrap();
    Rig {
        _ws: ws,
        herdr,
        daemon,
    }
}

fn whoami(r: &Rig, claim: Option<&str>) -> Result<Value, rpc::RpcError> {
    let mut req = Request::new("whoami", Value::Null);
    req.caller = claim.map(String::from);
    rpc::call(&r.daemon.socket, &req).unwrap().into_result()
}

/// Maps the test process to a fresh agent pane so the stop verb is let through.
fn stop(r: Rig) {
    r.herdr.clear(qq_core::herdr::Verb::PaneList);
    let pane = r.herdr.seed_pane("stop", None, &[]);
    r.herdr
        .agent_start("stopper", "claude", &pane, 0, &[])
        .unwrap();
    r.herdr.set_pane_pids(&pane, me(), &[]);
    r.daemon.stop().unwrap();
}

fn me() -> u32 {
    std::process::id()
}

#[test]
fn peer_in_a_worker_pane_is_that_worker() {
    let r = rig("peer-mapped");
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.agent_start("w-1", "claude", &pane, 0, &[]).unwrap();
    r.herdr.set_pane_pids(&pane, me(), &[]);
    assert_eq!(whoami(&r, None).unwrap()["caller"], "w-1");
    stop(r);
}

#[test]
fn spoofed_claim_is_replaced_by_the_peer_identity() {
    let r = rig("peer-spoof");
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.agent_start("w-1", "claude", &pane, 0, &[]).unwrap();
    r.herdr.set_pane_pids(&pane, me(), &[]);
    assert_eq!(whoami(&r, Some("cos")).unwrap()["caller"], "w-1");
    stop(r);
}

#[test]
fn descendant_of_the_pane_shell_maps_to_the_pane() {
    let r = rig("peer-ancestor");
    let pane = r
        .herdr
        .seed_pane("c", Some("claude"), &[("grid_role", "cos")]);
    let parent = parent_pid(me() as i32).expect("test process has a parent");
    r.herdr.set_pane_pids(&pane, parent as u32, &[]);
    assert_eq!(whoami(&r, None).unwrap()["caller"], "cos");
    stop(r);
}

#[test]
fn a_principal_is_refused_every_agent_verb_and_the_claim_does_not_save_it() {
    let r = rig("peer-unmapped");
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.agent_start("w-1", "claude", &pane, 0, &[]).unwrap();
    // The pane belongs to some other process tree, so the peer is the Principal.
    r.herdr.set_pane_pids(&pane, 1, &[]);
    let e = whoami(&r, Some("w-1")).unwrap_err();
    assert_eq!(e.code, "agent_only", "{e:?}");
    assert!(e.message.contains("owner"), "names the principal: {e:?}");
    stop(r);
}

/// Issue #375: the owner runs `quaqua` from a plain shell pane inside herdr, so
/// a pane that hosts no agent resolves exactly like no pane at all — the
/// read-only Principal. It reads, and every farm-changing verb is refused with
/// the CoS pane named as where to run it.
#[test]
fn a_pane_with_no_agent_is_the_read_only_principal() {
    let r = rig("peer-noagent");
    let pane = r.herdr.seed_pane("h", Some("zsh"), &[]);
    r.herdr.set_pane_pids(&pane, me(), &[]);
    // The reads answer, exactly as they do from a terminal outside herdr.
    assert_eq!(call(&r, "ping", None).unwrap()["pong"], true);
    assert!(call(&r, "status", None).is_ok(), "status");
    assert_eq!(call(&r, "roster", None).unwrap()["ran"], true);
    // The identity probe still names no agent, as for the plain Principal.
    assert_eq!(whoami(&r, None).unwrap_err().code, "agent_only");
    // A farm-changing verb is refused, and the message says where to run it.
    let e = call(&r, "stop", None).unwrap_err();
    assert_eq!(e.code, "agent_only", "{e:?}");
    assert!(
        e.message.contains("CoS pane"),
        "names where to run it: {e:?}"
    );
    // The same through the CLI, as the owner runs it: `quaqua ping` answers and
    // `quaqua stop` exits refused, naming the CoS pane.
    let runs = r._ws.runs_dir();
    let ping = qq(&runs, &["ping"]);
    assert_eq!(ping.status.code(), Some(0), "{ping:?}");
    assert!(
        String::from_utf8_lossy(&ping.stdout).contains("pong"),
        "{ping:?}"
    );
    let refused = qq(&runs, &["stop"]);
    assert_eq!(
        refused.status.code(),
        Some(qq_core::cli::EXIT_REFUSED),
        "{refused:?}"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("CoS pane"),
        "{refused:?}"
    );
    r.herdr.pane_close(&pane).unwrap();
    stop(r);
}

#[test]
fn herdr_failure_fails_closed() {
    let r = rig("peer-herdr-down");
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.agent_start("w-1", "claude", &pane, 0, &[]).unwrap();
    r.herdr.set_pane_pids(&pane, me(), &[]);
    r.herdr.set_fault(
        qq_core::herdr::Verb::PaneList,
        std::io::ErrorKind::BrokenPipe,
    );
    assert_eq!(whoami(&r, Some("w-1")).unwrap_err().code, "unmapped_peer");
    r.herdr.pane_close(&pane).unwrap();
    stop(r);
}

/// Owner decision 2026-11-25: outside herdr the Principal reads only, so a pane
/// herdr cannot describe is no longer a refusal — it may not be the peer's pane,
/// and the worst it can hide is a reader. A worker churn that makes a pane
/// undescribable must not stop the launcher's `ping` from answering.
#[test]
fn an_undescribable_other_pane_does_not_block_the_read_only_principal() {
    let r = rig("peer-procinfo-fault");
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.agent_start("w-1", "claude", &pane, 0, &[]).unwrap();
    r.herdr.set_pane_pids(&pane, me(), &[]);
    r.herdr.set_fault(
        qq_core::herdr::Verb::PaneProcessInfo,
        std::io::ErrorKind::BrokenPipe,
    );
    // The peer is in no pane; the one pane is undescribable.
    assert_eq!(call(&r, "ping", None).unwrap()["pong"], true);
    assert!(call(&r, "status", None).is_ok(), "status");
    // The writes stay closed to it, and the message says where to run them.
    let e = call(&r, "stop", None).unwrap_err();
    assert_eq!(e.code, "agent_only", "{e:?}");
    assert!(e.message.contains("herdr pane"), "{}", e.message);
    r.herdr.clear(qq_core::herdr::Verb::PaneProcessInfo);
    stop(r);
}

/// A peer outside every pane, as a human shell is.
fn unmapped(r: &Rig) {
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.set_pane_pids(&pane, 1, &[]);
}

fn call(r: &Rig, verb: &str, claim: Option<&str>) -> Result<Value, rpc::RpcError> {
    let mut req = Request::new(verb, Value::Null);
    req.caller = claim.map(String::from);
    rpc::call(&r.daemon.socket, &req).unwrap().into_result()
}

/// Issue #368, as narrowed by the owner on 2026-11-25: a peer in no pane is the
/// read-only Principal. It may look at the farm but not stop or change it —
/// those run from a herdr pane. The identity probe still names no agent, and a
/// mapped peer still wins.
#[test]
fn the_read_only_principal_may_ping_status_and_roster_but_not_stop() {
    let r = rig("peer-human");
    unmapped(&r);
    // A claim of another caller's name decides nothing: the gate names the peer.
    assert_eq!(call(&r, "ping", Some("cos")).unwrap()["pong"], true);
    assert!(call(&r, "status", None).is_ok(), "status");
    assert_eq!(call(&r, "roster", None).unwrap()["ran"], true);
    // Its handler is the one that refuses a caller it cannot name as an agent.
    assert_eq!(
        call(&r, "caller.lookup", None).unwrap_err().code,
        "unmapped_peer"
    );
    let e = call(&r, "stop", None).unwrap_err();
    assert_eq!(e.code, "agent_only", "{e:?}");
    assert!(e.message.contains("herdr pane"), "{}", e.message);
    // A mapped peer still reaches the same daemon, stop included.
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.agent_start("w-3", "claude", &pane, 0, &[]).unwrap();
    r.herdr.set_pane_pids(&pane, me(), &[]);
    assert_eq!(call(&r, "ping", None).unwrap()["pong"], true);
    assert_eq!(call(&r, "stop", None).unwrap()["stopping"], true);
    r.daemon.stop().unwrap();
}

/// The same through the CLI, which is how the launcher and the owner call it: a
/// write from a plain terminal exits 77 with a message naming the herdr pane.
#[test]
fn quaqua_stop_from_a_plain_terminal_is_refused_with_the_pane_message() {
    let r = rig("peer-cli-stop");
    unmapped(&r);
    let runs = r._ws.runs_dir();
    let refused = qq(&runs, &["stop"]);
    assert_eq!(
        refused.status.code(),
        Some(qq_core::cli::EXIT_REFUSED),
        "{refused:?}"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("herdr pane"),
        "{refused:?}"
    );
    // The refused stop left the farm up, and ping still answers with 0.
    let up = qq(&runs, &["ping"]);
    assert_eq!(up.status.code(), Some(0), "{up:?}");
    r.daemon.stop().unwrap();
}

#[test]
fn descendant_of_a_pane_process_may_ping_status_and_stop() {
    let r = rig("peer-desc-lifecycle");
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.agent_start("w-4", "claude", &pane, 0, &[]).unwrap();
    let parent = parent_pid(me() as i32).expect("test process has a parent");
    r.herdr.set_pane_pids(&pane, parent as u32, &[]);
    assert_eq!(call(&r, "ping", None).unwrap()["pong"], true);
    assert!(call(&r, "status", None).is_ok());
    call(&r, "stop", None).unwrap();
    r.daemon.stop().ok();
}

#[test]
fn the_principal_outside_a_pane_is_refused_agent_verbs_and_lookalikes() {
    let r = rig("peer-human-spawn");
    unmapped(&r);
    // Refused for every agent verb, and for every name the allowlist does not
    // match exactly: no prefix of an open verb opens a closed one.
    for v in [
        "task.start",
        "worker.stop",
        "mail.inbox",
        "mail.send",
        "pipeline",
        "Push",
        "mail.inbox.x",
        "pingx",
        "status.all",
    ] {
        assert_eq!(call(&r, v, None).unwrap_err().code, "agent_only", "{v}");
    }
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.agent_start("w-2", "claude", &pane, 0, &[]).unwrap();
    r.herdr.set_pane_pids(&pane, me(), &[]);
    assert_eq!(call(&r, "task.start", None).unwrap()["ran"], true);
    stop(r);
}

/// `quaqua <args>` from this process's own runs directory, as the owner's
/// terminal would run it.
fn qq(runs: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .args(args)
        .env("QUAQUA_RUNS_DIR", runs)
        .env("QUAQUA_TEST", "1")
        .output()
        .expect("run quaqua")
}

/// Issue #368, acceptance end to end: from a process in no herdr pane, `quaqua
/// ping` exits 0 while the daemon runs, exits with the unreachable code while it
/// is stopped, and with the refusal code (never 69) when the daemon refuses it —
/// so a launcher can tell "start the daemon" from "this caller may not".
#[test]
fn quaqua_ping_from_a_plain_terminal_answers_and_tells_its_two_failures_apart() {
    let r = rig("peer-cli");
    unmapped(&r);
    let runs = r._ws.runs_dir();
    let up = qq(&runs, &["ping"]);
    assert_eq!(up.status.code(), Some(0), "{up:?}");
    assert!(
        String::from_utf8_lossy(&up.stdout).contains("pong"),
        "{up:?}"
    );
    // The daemon answers and refuses this caller: not the unreachable code.
    let refused = qq(&runs, &["call", "mail.send", "{}"]);
    assert_eq!(
        refused.status.code(),
        Some(qq_core::cli::EXIT_REFUSED),
        "{refused:?}"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("agent_only"),
        "{refused:?}"
    );
    r.daemon.stop().unwrap();
    let down = qq(&runs, &["ping"]);
    assert_eq!(
        down.status.code(),
        Some(qq_core::cli::EXIT_UNREACHABLE),
        "{down:?}"
    );
}

fn cos_rows(r: &Rig) -> i64 {
    let conn = qq_core::db::open(&r._ws.runs_dir().join(qq_core::db::path::DB_FILE)).unwrap();
    conn.query_row(
        "SELECT count(*) FROM agent WHERE name = 'cos' AND role = 'cos' AND ended_ms IS NULL",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn first_call_from_the_cos_pane_makes_one_active_cos_agent() {
    let r = rig_with("cos-boot", true);
    let pane = r
        .herdr
        .seed_pane("c", Some("claude"), &[("grid_role", "cos")]);
    let parent = parent_pid(me() as i32).expect("test process has a parent");
    r.herdr.set_pane_pids(&pane, parent as u32, &[]);
    assert_eq!(cos_rows(&r), 0, "a fresh workspace has no cos row");
    for _ in 0..2 {
        assert_eq!(whoami(&r, None).unwrap()["caller"], "cos");
        assert_eq!(cos_rows(&r), 1, "one row, repeat calls add none");
    }
    stop(r);
}

#[test]
fn bootstrap_makes_no_cos_row_for_a_worker_pane_or_the_principal() {
    let r = rig_with("cos-boot-none", true);
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.agent_start("w-1", "claude", &pane, 0, &[]).unwrap();
    r.herdr.set_pane_pids(&pane, me(), &[]);
    assert_eq!(whoami(&r, Some("cos")).unwrap()["caller"], "w-1");
    assert_eq!(
        cos_rows(&r),
        0,
        "a claim of 'cos' from a worker pane seeds nothing"
    );
    r.herdr.set_pane_pids(&pane, 1, &[]);
    assert_eq!(whoami(&r, Some("cos")).unwrap_err().code, "agent_only");
    assert_eq!(cos_rows(&r), 0, "the principal seeds nothing");
    stop(r);
}
