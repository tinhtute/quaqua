//! R12: a phase-check rerun child that calls the daemon is never resolved as
//! `cos`. The rerun is a real child process (this test binary re-executed);
//! the fake herdr says which pane owns which pid, and the daemon names the
//! caller from the socket peer, whatever the child claims.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::identity::peer::HerdrPeers;
use qq_core::rpc::{self, Request};
use qq_core::testkit::TestWorkspace;
use serde_json::{json, Value};

const SOCK_ENV: &str = "QQ_RERUN_PROBE_SOCK";

/// Runs as the rerun child when `SOCK_ENV` is set, else does nothing. Waits for
/// a line on stdin (the parent has mapped its pid by then), claims to be `cos`
/// and asks the daemon who it is.
#[test]
fn rerun_isolation_probe_child() {
    let Ok(sock) = std::env::var(SOCK_ENV) else {
        return;
    };
    let mut go = String::new();
    std::io::stdin().read_line(&mut go).unwrap();
    let mut req = Request::new("caller.lookup", Value::Null);
    req.caller = Some("cos".into());
    let out = match rpc::call(std::path::Path::new(&sock), &req).map(|r| r.into_result()) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => json!({ "error": e.code }),
        Err(e) => json!({ "io": e.to_string() }),
    };
    println!("PROBE:{out}");
}

struct Rig {
    ws: TestWorkspace,
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
    d.require_peer(Arc::new(HerdrPeers::new(herdr.clone())));
    let daemon = d.spawn().unwrap();
    Rig { ws, herdr, daemon }
}

/// What the daemon says to a freshly spawned rerun child. `map` runs with the
/// child's pid before the child is let go, so a test can place that pid in a pane.
fn probe(r: &Rig, map: impl FnOnce(u32)) -> Value {
    let sock: PathBuf = rpc::socket_path(&r.ws.runs_dir());
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "rerun_isolation_probe_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SOCK_ENV, &sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    map(child.id());
    writeln!(child.stdin.take().unwrap()).unwrap();
    let lines: Vec<String> = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map_while(Result::ok)
        .collect();
    let out = lines
        .iter()
        .find_map(|l| l.split_once("PROBE:").map(|(_, v)| v.to_string()))
        .expect("probe child printed no answer");
    child.wait().unwrap();
    serde_json::from_str(&out).unwrap()
}

fn cos_pane(r: &Rig, shell: u32) {
    let p = r
        .herdr
        .seed_pane("c", Some("claude"), &[("grid_role", "cos")]);
    r.herdr.set_pane_pids(&p, shell, &[]);
}

fn stop(r: Rig) {
    // The test process is not a pane peer; stop the daemon in-process.
    r.daemon.stop().unwrap();
}

#[test]
fn rerun_isolation_control_a_child_under_the_cos_pane_is_cos() {
    // Why R12 exists: ancestry alone would hand the CoS identity to any child
    // the CoS pane starts, so a rerun must never be launched from there.
    let r = rig("rerun-control");
    cos_pane(&r, std::process::id());
    let v = probe(&r, |_| {});
    assert_eq!(v["cos"], true, "{v}");
    stop(r);
}

#[test]
fn rerun_isolation_child_in_a_verifier_pane_is_not_cos() {
    let r = rig("rerun-verifier");
    // The CoS pane is still this child's ancestor; the verifier pane is nearer.
    cos_pane(&r, std::process::id());
    let v = probe(&r, |child| {
        let p = r.herdr.seed_pane("v", None, &[]);
        r.herdr
            .agent_start("verifier-1", "claude", &p, 0, &[])
            .unwrap();
        r.herdr.set_pane_pids(&p, child, &[]);
    });
    assert_eq!(v["cos"], false, "{v}");
    assert_eq!(v["caller"], "verifier-1", "{v}");
    stop(r);
}

#[test]
fn rerun_isolation_daemon_run_child_outside_every_pane_is_unmapped() {
    let r = rig("rerun-daemon");
    // The CoS pane runs some other process tree (pid 1), as a daemon-run child
    // is outside it.
    cos_pane(&r, 1);
    let v = probe(&r, |_| {});
    assert_eq!(v["error"], "unmapped_peer", "{v}");
    assert!(v.get("cos").is_none(), "{v}");
    stop(r);
}
