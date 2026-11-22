//! T61: the backlog (R23): risk split, CoS and owner verdicts, the proposal id
//! on the task, and the time on every score row.

use qq_core::backlog::{
    classify, decide, file, record_score, task_from_proposal, BacklogError, Change, Decider,
    NewProposal, Risk, Surface, Text,
};
use qq_core::cli::{Env, Registry};
use qq_core::daemon::{Config, Daemon};
use qq_core::db;
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::identity::peer::HerdrPeers;
use qq_core::rpc::{self, Request};
use qq_core::testkit::TestWorkspace;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::Arc;

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

/// A daemon with the backlog installed whose socket peer (this process) is the
/// CoS or a worker, and the registry holding the same install.
fn rig(ws: &TestWorkspace, cos: bool) -> (qq_core::daemon::DaemonHandle, Registry) {
    let herdr = Arc::new(FakeHerdr::new());
    let pid = std::process::id();
    if cos {
        let p = herdr.seed_pane("c", Some("claude"), &[("grid_role", "cos")]);
        herdr.set_pane_pids(&p, pid, &[]);
    } else {
        let p = herdr.seed_pane("w", None, &[]);
        herdr.agent_start("w-1", "claude", &p, 0, &[]).unwrap();
        herdr.set_pane_pids(&p, pid, &[]);
    }
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone() as Arc<dyn HerdrClient>,
    });
    d.require_peer(Arc::new(HerdrPeers::new(herdr)));
    let mut reg = Registry::new();
    (qq_core::backlog::INSTALL)(&mut reg, &mut d).expect("backlog installs");
    (d.spawn().expect("daemon starts"), reg)
}

fn list(ws: &TestWorkspace) -> Vec<Value> {
    let r = rpc::call(&rpc::socket_path(&ws.runs_dir()), &Request::new("backlog.list", json!({})))
        .expect("transport")
        .into_result()
        .expect("list");
    r["proposals"].as_array().unwrap().clone()
}

fn run(reg: &Registry, env: &Env, args: &[&str]) -> i32 {
    let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    reg.dispatch(env, &a)
}

#[test]
fn backlog_commands_end_to_end() {
    let ws = TestWorkspace::new("t61b");
    ws.seal_herdr();
    let c = db::open(&ws.db_path()).unwrap();
    c.execute("INSERT INTO project (slug, created_ms) VALUES ('p', 1)", []).unwrap();
    let a = file(&c, &proposal(low(), None, Some(1)), 10).unwrap();
    let b = file(&c, &proposal(Change { external_effect: true, ..low() }, None, Some(1)), 11).unwrap();
    let d = file(&c, &proposal(low(), None, Some(1)), 12).unwrap();
    file(&c, &proposal(Change { surfaces: vec![Surface::Skill], ..low() }, None, Some(1)), 13).unwrap();
    let env = Env { runs_dir: ws.runs_dir() };

    // a worker is refused: no verdict is recorded
    let (worker, wreg) = rig(&ws, false);
    assert_eq!(run(&wreg, &env, &["backlog", "decide", &a.to_string(), "approve"]), 1);
    let refused = rpc::call(
        &rpc::socket_path(&ws.runs_dir()),
        &Request::new("backlog.decide", json!({ "id": a, "verdict": "approve" })),
    )
    .unwrap();
    assert_eq!(refused.error.unwrap().code, "refused");
    assert_eq!(list(&ws).iter().filter(|p| p["status"] == "open").count(), 4);
    worker.stop().unwrap();

    let (cos, reg) = rig(&ws, true);
    // the list names id, loop, kind, risk, surface, status; no task yet
    let rows = list(&ws);
    assert_eq!(rows.len(), 4);
    assert_eq!((rows[0]["id"].as_i64(), rows[0]["risk"].as_str(), rows[0]["loop"].as_i64()), (Some(a), Some("low"), Some(1)));
    assert_eq!(rows[1]["risk"], "high");
    assert_eq!(rows[3]["surface"], "skill");
    assert!(rows[..3].iter().all(|p| p["surface"].is_null()));
    assert!(rows.iter().all(|p| p["status"] == "open" && p["task_id"].is_null()));
    assert_eq!(run(&reg, &env, &["backlog", "list"]), 0);
    assert_eq!(run(&reg, &env, &["backlog", "list", "bogus"]), 1);

    // CoS approves low: a task carrying the proposal id appears
    assert_eq!(run(&reg, &env, &["backlog", "decide", &a.to_string(), "approve", "--title", "fix gate", "--note", "ok"]), 0);
    let (title, proposal_id): (String, i64) = c
        .query_row("SELECT title, proposal_id FROM task", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((title.as_str(), proposal_id), ("fix gate", a));
    // a decided proposal is not decided twice
    assert_eq!(run(&reg, &env, &["backlog", "decide", &a.to_string(), "reject"]), 1);
    // high stays open and makes no task, even when the CoS approves
    assert_eq!(run(&reg, &env, &["backlog", "decide", &b.to_string(), "approve"]), 1);
    // reject records a verdict and makes no task
    assert_eq!(run(&reg, &env, &["backlog", "decide", &d.to_string(), "reject", "--note", "no"]), 0);
    // bad input is refused before any write
    assert_eq!(run(&reg, &env, &["backlog", "decide", "999", "approve"]), 1);
    assert_eq!(run(&reg, &env, &["backlog", "decide", &d.to_string(), "maybe"]), 1);

    let rows = list(&ws);
    let by = |id: i64| rows.iter().find(|p| p["id"] == id).unwrap().clone();
    assert_eq!((by(a)["status"].as_str(), by(a)["task_id"].is_null()), (Some("approved"), false));
    assert_eq!((by(b)["status"].as_str(), by(b)["task_id"].is_null()), (Some("open"), true));
    assert_eq!((by(d)["status"].as_str(), by(d)["task_id"].is_null()), (Some("rejected"), true));
    let verdicts: Vec<(i64, String, String)> = c
        .prepare("SELECT proposal_id, decider_role, verdict FROM decision ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(verdicts, vec![(a, "cos".into(), "approve".into()), (d, "cos".into(), "reject".into())]);
    let tasks: i64 = c.query_row("SELECT COUNT(*) FROM task", [], |r| r.get(0)).unwrap();
    assert_eq!(tasks, 1);
    cos.stop().unwrap();
    ws.finish();
}
