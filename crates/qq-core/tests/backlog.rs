//! T61: the backlog (R23): risk split, CoS and owner verdicts, the proposal id
//! on the task, and the time on every score row.

use qq_core::backlog::{
    classify, decide, file, record_score, task_from_proposal, BacklogError, Change, Decider,
    NewProposal, Risk, Surface, Text,
};
use qq_core::db;
use rusqlite::Connection;

fn conn() -> (Connection, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("qq-t61-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    let c = db::open(&dir.join("t61.db")).expect("open");
    c.execute("INSERT INTO project (slug, created_ms) VALUES ('p', 1)", []).unwrap();
    (c, dir)
}

fn low() -> Change {
    Change { reversible: true, projects: 1, surfaces: vec![], external_effect: false }
}

fn proposal(change: Change, floor: Option<Risk>, project: Option<i64>) -> NewProposal {
    NewProposal {
        loop_no: 1,
        project_id: project,
        change,
        protocol_floor: floor,
        text: Text { observation: Some("gate failed".into()), ..Text::default() },
    }
}

fn risk_of(c: &Connection, id: i64) -> String {
    c.query_row("SELECT risk FROM proposal WHERE id = ?1", [id], |r| r.get(0)).unwrap()
}

#[test]
fn backlog_risk_split() {
    let (mut c, dir) = conn();
    // low: reversible, one project, no surface, no external effect
    assert_eq!(classify(&low(), None), Risk::Low);
    let a = file(&c, &proposal(low(), None, Some(1)), 10).unwrap();
    assert_eq!(risk_of(&c, a), "low");
    // each other condition alone makes it high
    let irreversible = Change { reversible: false, ..low() };
    let two = Change { projects: 2, ..low() };
    let none = Change { projects: 0, ..low() };
    let external = Change { external_effect: true, ..low() };
    for ch in [irreversible, two, none, external] {
        assert_eq!(classify(&ch, None), Risk::High);
    }
    // no project named: not inside one project
    let b = file(&c, &proposal(low(), None, None), 11).unwrap();
    assert_eq!(risk_of(&c, b), "high");
    // a protocol can only tighten: floor high lifts low, floor low never lowers high
    assert_eq!(classify(&low(), Some(Risk::High)), Risk::High);
    assert_eq!(classify(&Change { external_effect: true, ..low() }, Some(Risk::Low)), Risk::High);

    // CoS decides low with a recorded verdict; high waits for the owner
    decide(&mut c, a, Decider::Cos, true, Some("ok"), 20).unwrap();
    let (role, verdict, ts): (String, String, i64) = c
        .query_row("SELECT decider_role, verdict, ts_ms FROM decision WHERE proposal_id = ?1", [a], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((role.as_str(), verdict.as_str(), ts), ("cos", "approve", 20));
    assert_eq!(decide(&mut c, b, Decider::Cos, true, None, 21), Err(BacklogError::OwnerOnly(b)));
    let status: String = c.query_row("SELECT status FROM proposal WHERE id = ?1", [b], |r| r.get(0)).unwrap();
    assert_eq!(status, "open");
    decide(&mut c, b, Decider::Owner, false, None, 22).unwrap();
    assert_eq!(decide(&mut c, b, Decider::Owner, true, None, 23), Err(BacklogError::NotOpen(b)));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rule_change_always_high() {
    let (c, dir) = conn();
    for s in [Surface::Rule, Surface::Skill, Surface::Config, Surface::Protocol] {
        let ch = Change { surfaces: vec![s], ..low() };
        assert_eq!(classify(&ch, None), Risk::High, "{s:?}");
        let id = file(&c, &proposal(ch, Some(Risk::Low), Some(1)), 1).unwrap();
        assert_eq!(risk_of(&c, id), "high", "{s:?}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn proposal_task_link_recorded() {
    let (mut c, dir) = conn();
    let id = file(&c, &proposal(low(), None, Some(1)), 10).unwrap();
    // an open proposal does not become a task
    assert_eq!(
        task_from_proposal(&mut c, id, "fix", "engineer", None, 11),
        Err(BacklogError::NotApproved(id))
    );
    decide(&mut c, id, Decider::Cos, true, None, 12).unwrap();
    let t = task_from_proposal(&mut c, id, "fix", "engineer", None, 13).unwrap();
    let (pid, project): (i64, i64) = c
        .query_row("SELECT proposal_id, project_id FROM task WHERE id = ?1", [t], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((pid, project), (id, 1));
    // idempotent
    assert_eq!(task_from_proposal(&mut c, id, "fix", "engineer", None, 14).unwrap(), t);
    // a rejected proposal never becomes a task
    let r = file(&c, &proposal(low(), None, Some(1)), 15).unwrap();
    decide(&mut c, r, Decider::Cos, false, None, 16).unwrap();
    assert_eq!(
        task_from_proposal(&mut c, r, "x", "engineer", None, 17),
        Err(BacklogError::NotApproved(r))
    );
    // every score row carries its time
    c.execute("INSERT INTO agent (name, role, started_ms) VALUES ('a', 'peer', 1)", []).unwrap();
    c.execute("INSERT INTO harness (name, driver) VALUES ('claude', 'native')", []).unwrap();
    c.execute("INSERT INTO kind (name, harness, model) VALUES ('k', 'claude', 'm')", []).unwrap();
    c.execute(
        "INSERT INTO run (agent_id, kind, harness, model, started_ms, cost_state) VALUES (1, 'k', 'claude', 'm', 1, 'unknown')",
        [],
    )
    .unwrap();
    let s1 = record_score(&c, 1, "rubric", 0.5, None, 100).unwrap();
    let s2 = record_score(&c, 1, "rubric", 0.9, Some("{}"), 200).unwrap();
    let ts: Vec<i64> = [s1, s2]
        .iter()
        .map(|s| c.query_row("SELECT ts_ms FROM score WHERE id = ?1", [s], |r| r.get(0)).unwrap())
        .collect();
    assert_eq!(ts, vec![100, 200]);
    let _ = std::fs::remove_dir_all(dir);
}
