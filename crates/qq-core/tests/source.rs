//! T62: the source interface and the schedule-driven pull (R24).

use std::path::{Path, PathBuf};

use qq_core::config;
use qq_core::daemon::schedule;
use qq_core::db;
use qq_core::source::{FailureSource, IdeaInbox, Source, FAILURE_SOURCE, IDEA_SOURCE};
use rusqlite::Connection;

struct Fx {
    conn: Connection,
    dir: PathBuf,
}

fn fx(tag: &str) -> Fx {
    let dir = std::env::temp_dir().join(format!("qq-t62-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inbox/ideas")).unwrap();
    let mut conn = db::open(&dir.join("t62.db")).expect("open");
    let team = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
    )
    .unwrap();
    config::seed(&mut conn, &team).expect("seed");
    conn.execute("INSERT INTO project(slug, created_ms) VALUES ('p', 1)", [])
        .unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS events (workflow_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         ts_ms INTEGER NOT NULL, kind TEXT NOT NULL, payload_json TEXT NOT NULL, \
         PRIMARY KEY (workflow_id, seq))",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS snapshots (workflow_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         state_json TEXT NOT NULL, PRIMARY KEY (workflow_id, seq))",
        [],
    )
    .unwrap();
    Fx { conn, dir }
}

fn gate_event(conn: &Connection, wf: &str, seq: i64, kind: &str, reason: &str) {
    // As production: the workflow row carries no project or task; the pipeline
    // input is journaled in the snapshot.
    conn.execute(
        "INSERT OR IGNORE INTO workflow (id, kind, state, started_ms, updated_ms) \
         VALUES (?1, 'pipeline', 'running', 1, 1)",
        [wf],
    )
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO snapshots (workflow_id, seq, state_json) VALUES (?1, 1, ?2)",
        rusqlite::params![
            wf,
            serde_json::json!({ "input": { "project_id": 1 } }).to_string()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (workflow_id, seq, ts_ms, kind, payload_json) VALUES (?1, ?2, 1, ?3, ?4)",
        rusqlite::params![wf, seq, kind, serde_json::json!({ "reason": reason }).to_string()],
    )
    .unwrap();
}

fn proposals(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM proposal", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn loop1_gate_failure_files_proposal() {
    let Fx { mut conn, dir } = fx("l1");
    // noise: other pauses, another merge refusal, a successful event
    gate_event(
        &conn,
        "wf-ok",
        1,
        "workflow_completed",
        "blocked 4: check_failed: x",
    );
    gate_event(
        &conn,
        "wf-ok",
        2,
        "pause_for_approval",
        "the report ends in BLOCKED",
    );
    gate_event(
        &conn,
        "wf-ok",
        3,
        "pause_for_approval",
        "blocked 4: merge_conflict: a.rs",
    );
    gate_event(
        &conn,
        "wf-bad",
        1,
        "pause_for_approval",
        "blocked 4: check_failed: cargo test red",
    );
    schedule::add(&conn, FAILURE_SOURCE, 1000, 0).unwrap();
    let src = FailureSource;
    let ids = schedule::pull_due(&mut conn, &[&src], 0).unwrap();
    assert_eq!(ids.len(), 1, "only the gate failure files");
    let (loop_no, risk, project, obs, ev): (i64, String, Option<i64>, String, String) = conn
        .query_row(
            "SELECT loop, risk, project_id, observation, evidence FROM proposal WHERE id = ?1",
            [ids[0]],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!((loop_no, risk.as_str(), project), (1, "low", Some(1)));
    assert!(obs.contains("wf-bad") && obs.contains("check_failed"));
    assert!(ev.contains("cargo test red"));
    // once: later pulls file nothing, and the same failure again (a retry) is the same signature
    assert!(schedule::pull_due(&mut conn, &[&src], 0)
        .unwrap()
        .is_empty());
    assert!(schedule::pull_due(&mut conn, &[&src], 5000)
        .unwrap()
        .is_empty());
    gate_event(
        &conn,
        "wf-bad",
        2,
        "pause_for_approval",
        "blocked 4: check_failed: cargo test red again",
    );
    assert!(schedule::pull_due(&mut conn, &[&src], 6000)
        .unwrap()
        .is_empty());
    assert_eq!(proposals(&conn), 1);
    // a different failure word, or another workflow, files its own proposal
    gate_event(
        &conn,
        "wf-bad",
        3,
        "pause_for_approval",
        "blocked 4: base_broken: red on base",
    );
    gate_event(
        &conn,
        "wf-two",
        1,
        "pause_for_approval",
        "blocked 4: check_failed: lint",
    );
    assert_eq!(
        schedule::pull_due(&mut conn, &[&src], 9000).unwrap().len(),
        2
    );
    assert_eq!(proposals(&conn), 3);
    // a workflow with no snapshot falls back to the task's project
    conn.execute("INSERT INTO workflow (id, kind, state, started_ms, updated_ms) VALUES ('wf-t', 'pipeline', 'running', 1, 1)", []).unwrap();
    conn.execute("INSERT INTO events VALUES ('wf-t', 1, 1, 'pause_for_approval', '{\"reason\":\"blocked 4: check_failed: x\"}')", []).unwrap();
    assert_eq!(
        src.pull(&conn)
            .unwrap()
            .iter()
            .find(|i| i.key == "wf-t:check_failed")
            .unwrap()
            .project_id,
        None
    );
    conn.execute("INSERT INTO task (project_id, title, disposition, state, created_ms) VALUES (1, 't', 'engineer', 'open', 1)", []).unwrap();
    conn.execute(
        "INSERT INTO snapshots VALUES ('wf-t', 1, '{\"input\":{\"task_id\":1}}')",
        [],
    )
    .unwrap();
    assert_eq!(
        src.pull(&conn)
            .unwrap()
            .iter()
            .find(|i| i.key == "wf-t:check_failed")
            .unwrap()
            .project_id,
        Some(1)
    );
    // a database with no journal yet pulls nothing
    let bare = db::open(&dir.join("bare.db")).unwrap();
    assert!(src.pull(&bare).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn loop3_inbox_files_proposal() {
    let Fx { mut conn, dir } = fx("l3");
    let ideas = dir.join("inbox/ideas");
    std::fs::write(
        ideas.join("dark-mode.md"),
        "# Dark mode\n\nThe grid view should have one.\n",
    )
    .unwrap();
    std::fs::write(ideas.join("notes.txt"), "not an idea").unwrap();
    std::fs::write(ideas.join("empty.md"), "  \n").unwrap();
    schedule::add(&conn, IDEA_SOURCE, 1000, 0).unwrap();
    let src = IdeaInbox::new(&dir);
    let ids = schedule::pull_due(&mut conn, &[&src], 0).unwrap();
    assert_eq!(ids.len(), 1);
    let (loop_no, risk, status, obs, ev): (i64, String, String, String, String) = conn
        .query_row(
            "SELECT loop, risk, status, observation, evidence FROM proposal WHERE id = ?1",
            [ids[0]],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        (loop_no, risk.as_str(), status.as_str()),
        (3, "high", "open")
    );
    assert!(obs.contains("The grid view should have one."));
    assert_eq!(ev, "inbox/ideas/dark-mode.md");
    // once, even when the file is edited; a new file files its own
    std::fs::write(ideas.join("dark-mode.md"), "# Dark mode\nedited\n").unwrap();
    assert!(schedule::pull_due(&mut conn, &[&src], 2000)
        .unwrap()
        .is_empty());
    std::fs::write(ideas.join("export.md"), "Export runs as CSV\n").unwrap();
    assert_eq!(
        schedule::pull_due(&mut conn, &[&src], 4000).unwrap().len(),
        1
    );
    assert_eq!(proposals(&conn), 2);
    // a long idea is cut, a symlink is not read
    std::fs::write(ideas.join("long.md"), "x".repeat(50_000)).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/hosts", ideas.join("link.md")).unwrap();
    let items = src.pull(&conn).unwrap();
    let long = items.iter().find(|i| i.key == "long.md").unwrap();
    assert_eq!(long.text.observation.as_ref().unwrap().len(), 20_000);
    assert!(items.iter().all(|i| i.key != "link.md"));
    // a missing inbox is an empty inbox
    let none = IdeaInbox::new(&dir.join("nowhere"));
    assert!(none.pull(&conn).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn trigger_schedule_pull_only() {
    let Fx { mut conn, dir } = fx("tr");
    let src = FailureSource;
    gate_event(
        &conn,
        "wf-bad",
        1,
        "pause_for_approval",
        "blocked 4: check_failed: red",
    );
    // no schedule row: nothing is pulled, whatever the clock says
    assert!(schedule::pull_due(&mut conn, &[&src], i64::MAX / 2)
        .unwrap()
        .is_empty());
    assert_eq!(proposals(&conn), 0);
    // a schedule not yet due pulls nothing; due, it pulls once; the clock pair moves by the interval
    schedule::add(&conn, FAILURE_SOURCE, 1000, 500).unwrap();
    assert_eq!(schedule::next_deadline(&conn).unwrap(), Some(500));
    assert!(schedule::pull_due(&mut conn, &[&src], 499)
        .unwrap()
        .is_empty());
    assert_eq!(
        schedule::pull_due(&mut conn, &[&src], 500).unwrap().len(),
        1
    );
    assert_eq!(schedule::next_deadline(&conn).unwrap(), Some(1500));
    // a schedule whose source is not given is skipped and moves on
    schedule::add(&conn, "unknown", 10, 0).unwrap();
    assert!(schedule::pull_due(&mut conn, &[&src], 600)
        .unwrap()
        .is_empty());
    assert!(schedule::due(&conn, 600).unwrap().is_empty());
    // no listening endpoint in the source or schedule modules
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for f in ["source/mod.rs", "daemon/schedule.rs"] {
        let text = std::fs::read_to_string(root.join(f)).unwrap();
        for needle in [
            "Listener",
            "::bind(",
            "hyper",
            "axum",
            "tiny_http",
            "webhook",
        ] {
            let hits = text
                .lines()
                .filter(|l| !l.trim_start().starts_with("//") && l.contains(needle))
                .count();
            assert_eq!(hits, 0, "{f} must not use {needle}");
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}
