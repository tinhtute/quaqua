//! Phase notify T2: the cos-inbox digest says "daemon down" once per session
//! while the daemon cannot answer, and stays silent otherwise.

use std::sync::Arc;

use qq_core::cli::{Env, Registry};
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::herdr::FakeHerdr;
use qq_core::memory;
use qq_core::rpc::{self, Request};
use qq_core::testkit::TestWorkspace;
use serde_json::{json, Value};

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn start(ws: &TestWorkspace) -> DaemonHandle {
    let conn = qq_core::db::open(&ws.db_path()).expect("open qq db");
    conn.execute(
        "INSERT OR IGNORE INTO topology_edge (from_role, to_role, edge) VALUES ('lead', 'peer', 'mail')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO project (slug, created_ms) VALUES ('p1', 0)", [])
        .unwrap();
    for (name, role) in [("lead", "lead"), ("peer-a", "peer")] {
        conn.execute(
            "INSERT INTO agent (name, role, project_id, started_ms) VALUES (?1, ?2, 1, 1)",
            rusqlite::params![name, role],
        )
        .unwrap();
    }
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: Arc::new(FakeHerdr::new()),
    });
    qq_core::bus::install(&mut Registry::new(), &mut d).expect("bus installs");
    d.spawn().expect("daemon starts")
}

fn send(ws: &TestWorkspace, kind: &str, body: &str) {
    let mut req = Request::new("mail.send", json!({"to": "peer-a", "body": body, "kind": kind}));
    req.caller = Some("lead".into());
    rpc::call(&rpc::socket_path(&ws.runs_dir()), &req)
        .expect("transport")
        .into_result()
        .expect("mail.send");
}

fn hook(env: &Env, extra: &[&str]) -> String {
    let mut a = vec!["--for", "peer-a", "--format", "hook-ups"];
    a.extend_from_slice(extra);
    memory::inbox_as(env, Some("peer-a".to_string()), &args(&a)).expect("hook fails open")
}

fn context(out: &str) -> String {
    let v: Value = serde_json::from_str(out).expect("hook json");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn inbox_daemon_down_state_mail_shows_in_digest() {
    let ws = TestWorkspace::new("inbox-down-state");
    let env = Env { runs_dir: ws.runs_dir() };
    let daemon = start(&ws);
    send(&ws, "state", "lead-1 stalled");
    let out = context(&hook(&env, &["--session", "s1"]));
    assert!(out.contains("[state] lead-1 stalled"), "{out}");
    assert!(!out.contains("daemon down"), "{out}");
    daemon.stop().unwrap();
}

#[test]
fn inbox_daemon_down_no_change_is_empty() {
    let ws = TestWorkspace::new("inbox-down-quiet");
    let env = Env { runs_dir: ws.runs_dir() };
    let daemon = start(&ws);
    assert_eq!(hook(&env, &["--session", "s1"]), "");
    assert_eq!(hook(&env, &["--session", "s1"]), "");
    daemon.stop().unwrap();
}

#[test]
fn inbox_daemon_down_prints_one_line_once_per_session() {
    let ws = TestWorkspace::new("inbox-down-once");
    let env = Env { runs_dir: ws.runs_dir() };
    let daemon = start(&ws);
    assert_eq!(hook(&env, &["--session", "s1"]), "");
    daemon.stop().unwrap();

    let first = context(&hook(&env, &["--session", "s1"]));
    assert_eq!(first.lines().count(), 1, "{first}");
    assert!(first.contains("daemon down"), "{first}");
    assert_eq!(hook(&env, &["--session", "s1"]), "", "once per session");
    let other = context(&hook(&env, &["--session", "s2"]));
    assert!(other.contains("daemon down"), "another session hears it once: {other}");

    // Stop-time never blocks on a dead daemon.
    let stop = memory::inbox_as(
        &env,
        Some("peer-a".into()),
        &args(&["--for", "peer-a", "--format", "hook-stop", "--session", "s3"]),
    )
    .unwrap();
    assert_eq!(stop, "");

    // Once the daemon answers again, a later outage is reported again.
    let daemon = start_again(&ws);
    assert_eq!(hook(&env, &["--session", "s1"]), "");
    daemon.stop().unwrap();
    let again = context(&hook(&env, &["--session", "s1"]));
    assert!(again.contains("daemon down"), "{again}");

    // Without a session id there is nothing to key the once-only marker on.
    assert_eq!(hook(&env, &[]), "");
}

fn start_again(ws: &TestWorkspace) -> DaemonHandle {
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: Arc::new(FakeHerdr::new()),
    });
    qq_core::bus::install(&mut Registry::new(), &mut d).expect("bus installs");
    d.spawn().expect("daemon restarts")
}
