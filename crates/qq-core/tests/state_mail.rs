//! R8: the daemon watches every engine-spawned agent and mails `cos` one
//! `kind=state` line per transition to stalled, blocked, done or exited.
//! Stalled, blocked and exited also raise a herdr notification.

use std::sync::Arc;
use std::time::{Duration, Instant};

use qq_core::cli::Registry;
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::herdr::{FakeHerdr, HerdrClient, Reply, Verb};
use qq_core::testkit::TestWorkspace;
use rusqlite::Connection;

/// Stall threshold and sweep period the cases inject through the workspace
/// settings, so no case waits for the 600 s default.
const STALL_SEC: f64 = 0.4;
const SWEEP_MS: u64 = 20;
/// How long an agent must stay missing before it counts as exited.
const EXIT_DEBOUNCE_SEC: f64 = 0.6;

struct Farm {
    ws: TestWorkspace,
    herdr: Arc<FakeHerdr>,
    _daemon: DaemonHandle,
}

fn db(ws: &TestWorkspace) -> Connection {
    qq_core::db::open(&ws.db_path()).expect("open qq db")
}

/// Starts a daemon whose workspace holds one open agent row per `(name, role)`
/// and the same agent running in the fake herdr, idle.
fn farm(tag: &str, agents: &[(&str, &str)]) -> Farm {
    let ws = TestWorkspace::new(tag);
    let herdr = Arc::new(FakeHerdr::new());
    {
        let conn = db(&ws);
        for (key, val) in [
            ("notify.stall_sec", STALL_SEC.to_string()),
            ("notify.sweep_ms", SWEEP_MS.to_string()),
            ("notify.exit_debounce_sec", EXIT_DEBOUNCE_SEC.to_string()),
        ] {
            conn.execute(
                "INSERT OR REPLACE INTO setting (key, value_json) VALUES (?1, ?2)",
                rusqlite::params![key, val],
            )
            .unwrap();
        }
        for (name, role) in agents {
            conn.execute(
                "INSERT INTO agent (name, role, started_ms) VALUES (?1, ?2, 1)",
                rusqlite::params![name, role],
            )
            .unwrap();
            let pane = herdr.seed_pane(name, None, &[]);
            herdr.agent_start(name, "claude", &pane, 1000, &[]).unwrap();
            herdr.set_status(name, "idle");
        }
    }
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone(),
    });
    qq_core::bus::install(&mut Registry::new(), &mut d).expect("bus installs");
    let daemon = d.spawn().expect("daemon starts");
    Farm {
        ws,
        herdr,
        _daemon: daemon,
    }
}

/// `(sender, kind, body)` of every mail to `cos`, oldest first.
fn cos_mail(ws: &TestWorkspace) -> Vec<(String, String, String)> {
    let conn = db(ws);
    let mut st = conn
        .prepare("SELECT sender, kind, body FROM mail WHERE recipient = 'cos' ORDER BY id")
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Long enough for several stall periods and sweeps to pass.
fn settle() {
    std::thread::sleep(Duration::from_secs_f64(STALL_SEC * 3.0));
}

#[test]
fn state_mail_stalled_lead_mails_and_notifies_once() {
    let f = farm("state-stalled", &[("hermes-lead", "lead")]);
    f.herdr.set_status("hermes-lead", "working");

    wait_for("the stalled mail", || !cos_mail(&f.ws).is_empty());
    settle();

    let mail = cos_mail(&f.ws);
    assert_eq!(mail.len(), 1, "one mail while it stays stalled: {mail:?}");
    assert_eq!(mail[0].0, "daemon");
    assert_eq!(mail[0].1, "state");
    assert!(
        mail[0].2.contains("hermes-lead") && mail[0].2.contains("stalled"),
        "{}",
        mail[0].2
    );
    assert_eq!(f.herdr.notifications().len(), 1);
}

#[test]
fn state_mail_exited_peer_mails_and_notifies_once() {
    let f = farm("state-exited", &[("sude-x", "peer")]);
    f.herdr.remove("sude-x");

    wait_for("the exited mail", || !cos_mail(&f.ws).is_empty());
    settle();

    let mail = cos_mail(&f.ws);
    assert_eq!(mail.len(), 1, "one mail for one exit: {mail:?}");
    assert_eq!(
        (mail[0].0.as_str(), mail[0].1.as_str()),
        ("daemon", "state")
    );
    assert!(
        mail[0].2.contains("sude-x") && mail[0].2.contains("exited"),
        "{}",
        mail[0].2
    );
    assert_eq!(f.herdr.notifications().len(), 1);
}

#[test]
fn state_mail_cleanup_exit_is_silent() {
    let f = farm("state-cleanup", &[("sude-y", "peer")]);
    db(&f.ws)
        .execute("UPDATE agent SET ended_ms = 2 WHERE name = 'sude-y'", [])
        .unwrap();
    f.herdr.remove("sude-y");

    settle();

    assert!(cos_mail(&f.ws).is_empty());
    assert!(f.herdr.notifications().is_empty());
}

#[test]
fn state_mail_blocked_mails_and_notifies() {
    let f = farm("state-blocked", &[("sude-b", "peer")]);
    f.herdr.set_status("sude-b", "blocked");

    wait_for("the blocked mail", || !cos_mail(&f.ws).is_empty());
    settle();

    let mail = cos_mail(&f.ws);
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert!(mail[0].2.contains("blocked"), "{}", mail[0].2);
    assert_eq!(f.herdr.notifications().len(), 1);
}

#[test]
fn state_mail_done_mails_without_a_notification() {
    let f = farm("state-done", &[("sude-d", "peer")]);
    f.herdr.set_status("sude-d", "done");

    wait_for("the done mail", || !cos_mail(&f.ws).is_empty());
    settle();

    let mail = cos_mail(&f.ws);
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert!(mail[0].2.contains("done"), "{}", mail[0].2);
    assert!(f.herdr.notifications().is_empty());
}

#[test]
fn state_mail_idle_working_flips_are_silent() {
    let f = farm("state-flip", &[("sude-f", "peer")]);
    for status in ["working", "idle", "working", "idle"] {
        f.herdr.set_status("sude-f", status);
        std::thread::sleep(Duration::from_millis(100));
    }

    settle();

    assert!(cos_mail(&f.ws).is_empty());
    assert!(f.herdr.notifications().is_empty());
}

/// Cleanup closes the pane first and the row after the worktree is gone, so a
/// sweep in between sees a missing agent with an open row.
#[test]
fn state_mail_exit_before_the_row_closes_is_silent() {
    let f = farm("state-cleanup-race", &[("sude-r", "peer")]);
    f.herdr.remove("sude-r");
    std::thread::sleep(Duration::from_millis(100));
    db(&f.ws)
        .execute("UPDATE agent SET ended_ms = 2 WHERE name = 'sude-r'", [])
        .unwrap();

    std::thread::sleep(Duration::from_secs_f64(EXIT_DEBOUNCE_SEC * 2.0));

    assert!(cos_mail(&f.ws).is_empty());
    assert!(f.herdr.notifications().is_empty());
}

#[test]
fn state_mail_huge_stall_setting_does_not_kill_the_sweeper() {
    let f = farm("state-huge", &[("sude-h", "peer")]);
    db(&f.ws)
        .execute(
            "INSERT OR REPLACE INTO setting (key, value_json) VALUES ('notify.stall_sec', '1e308')",
            [],
        )
        .unwrap();
    f.herdr.remove("sude-h");

    wait_for("the exited mail", || !cos_mail(&f.ws).is_empty());
}

#[test]
fn state_mail_failed_notification_leaves_a_trace_for_the_cos() {
    let f = farm("state-notify-fails", &[("sude-n", "peer")]);
    f.herdr
        .set_fault(Verb::NotificationShow, std::io::ErrorKind::Unsupported);
    f.herdr.set_status("sude-n", "blocked");

    wait_for("the trace mail", || cos_mail(&f.ws).len() >= 2);
    settle();

    let mail = cos_mail(&f.ws);
    assert_eq!(mail.len(), 2, "state line plus one trace: {mail:?}");
    assert!(mail[0].2.contains("sude-n") && mail[0].2.contains("blocked"));
    assert_eq!(
        (mail[1].0.as_str(), mail[1].1.as_str()),
        ("daemon", "state")
    );
    assert!(
        mail[1].2.contains("notification") && mail[1].2.contains("failed"),
        "{}",
        mail[1].2
    );
    assert!(f.herdr.notifications().is_empty());
}

#[test]
fn state_mail_refused_notification_leaves_a_trace_for_the_cos() {
    let f = farm("state-notify-refused", &[("sude-r", "peer")]);
    f.herdr
        .set_reply(Verb::NotificationShow, Reply::err("no_client"));
    f.herdr.set_status("sude-r", "blocked");

    wait_for("the trace mail", || cos_mail(&f.ws).len() >= 2);

    let mail = cos_mail(&f.ws);
    assert!(
        mail[1].2.contains("failed") && mail[1].2.contains("no_client"),
        "{mail:?}"
    );
}
