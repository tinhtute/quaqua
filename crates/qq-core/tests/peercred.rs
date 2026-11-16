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
    d.require_peer(Arc::new(HerdrPeers::new(herdr.clone())));
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
fn unmapped_peer_is_refused_and_the_claim_does_not_save_it() {
    let r = rig("peer-unmapped");
    let pane = r.herdr.seed_pane("w", None, &[]);
    r.herdr.agent_start("w-1", "claude", &pane, 0, &[]).unwrap();
    // The pane belongs to some other process tree.
    r.herdr.set_pane_pids(&pane, 1, &[]);
    let e = whoami(&r, Some("w-1")).unwrap_err();
    assert_eq!(e.code, "unmapped_peer");
    stop(r);
}

#[test]
fn mapped_pane_without_an_agent_is_refused() {
    let r = rig("peer-noagent");
    let pane = r.herdr.seed_pane("h", Some("zsh"), &[]);
    r.herdr.set_pane_pids(&pane, me(), &[]);
    assert_eq!(whoami(&r, None).unwrap_err().code, "unmapped_peer");
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
