//! T63: the Supervisor jobs, the read-only agent and the sensor order. The
//! scorer and the agent are fakes here: no model is called.

use std::cell::RefCell;
use std::sync::Arc;

use qq_core::auditor::AuditorRunOptions;
use qq_core::authority::{
    self, check_mail, check_question, check_spawn, Decision, Role,
};
use qq_core::backlog::{Change, Text};
use qq_core::cli::{Env, Registry};
use qq_core::config;
use qq_core::daemon::{Config, Daemon};
use qq_core::db;
use qq_core::herdr::FakeHerdr;
use qq_core::skills;
use qq_core::supervisor::agent::{
    agent_tools, brief, permit, Answer, AgentAction, AgentRunner, Finding, TemplateAgent,
};
use qq_core::supervisor::job::{
    run_outer2, score_runs, ArchRule, CommandCheck, RunView, Scorer, Sensors, Verdict,
};
use qq_core::supervisor::rubric::{self, Rubric};
use qq_core::supervisor::sensors::{order, Kind, LOOPS};
use qq_core::testkit::TestWorkspace;
use rusqlite::{params, Connection};

fn template() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml");
    std::fs::read_to_string(p).expect("read template team.toml")
}

/// A database seeded from the shipped team.toml with one project.
fn setup(ws: &TestWorkspace) -> Connection {
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    config::seed(&mut conn, &template()).expect("seed template");
    conn.execute("INSERT INTO project (slug, created_ms) VALUES ('alpha', 0)", [])
        .unwrap();
    conn
}

fn agent_row(conn: &Connection, name: &str, role: &str, project: Option<i64>, spawner: Option<i64>) -> i64 {
    conn.execute(
        "INSERT INTO agent (name, role, project_id, spawner_id, started_ms) VALUES (?1, ?2, ?3, ?4, 1)",
        params![name, role, project, spawner],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// One finished run with a one-line transcript; `in_project` ties it to a task of project 1.
fn finished_run(conn: &Connection, tag: &str, in_project: bool) -> i64 {
    let agent = agent_row(conn, &format!("w-{tag}"), "peer", Some(1), None);
    let kind: String = conn.query_row("SELECT name FROM kind LIMIT 1", [], |r| r.get(0)).unwrap();
    let task: Option<i64> = if in_project {
        conn.execute(
            "INSERT INTO task (project_id, title, disposition, state, created_ms) \
             VALUES (1, 't', 'engineer', 'open', 0)",
            [],
        )
        .unwrap();
        Some(conn.last_insert_rowid())
    } else {
        None
    };
    conn.execute(
        "INSERT INTO run (task_id, agent_id, kind, harness, model, started_ms, ended_ms, outcome, cost_state) \
         VALUES (?1, ?2, ?3, 'claude', 'm', 1, 2, 'done', 'unknown')",
        params![task, agent, kind],
    )
    .unwrap();
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO run_transcript (run_id, seq, ts_ms, role, content) VALUES (?1, 0, 1, 'worker', 'I reviewed my own change and merged it')",
        [id],
    )
    .unwrap();
    id
}

/// Scores 0 with a quote on the named rubrics, 2 elsewhere.
struct Fake {
    low: Vec<&'static str>,
    quote: Option<&'static str>,
    calls: RefCell<Vec<(i64, String)>>,
}

impl Fake {
    fn new(low: &[&'static str], quote: Option<&'static str>) -> Self {
        Fake { low: low.to_vec(), quote, calls: RefCell::new(vec![]) }
    }
}

impl Scorer for Fake {
    fn score(&self, rubric: &Rubric, run: &RunView) -> Result<Verdict, String> {
        self.calls.borrow_mut().push((run.run_id, rubric.id.clone()));
        if self.low.contains(&rubric.id.as_str()) {
            Ok(Verdict { value: 0, quote: self.quote.map(str::to_string) })
        } else {
            Ok(Verdict { value: 2, quote: None })
        }
    }
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn supervisor_scores_and_proposes() {
    // the default set is the 17 anti-patterns, one criterion each
    let set = rubric::defaults();
    assert_eq!(set.len(), 17);
    assert_eq!(set[0].id, "sheep compliance");
    assert_eq!(set[16].id, "supervisor overreach");
    assert!(set.iter().all(|r| !r.criterion.is_empty()));

    let ws = TestWorkspace::new("sup-score");
    let mut conn = setup(&ws);
    let r1 = finished_run(&conn, "a", true);
    let r2 = finished_run(&conn, "b", false);

    let scorer = Fake::new(&["self-acceptance"], Some("I reviewed my own change"));
    let rep = score_runs(&mut conn, &set, &scorer, &TemplateAgent, 100).unwrap();
    assert_eq!(rep.scored, 34, "17 rubrics on 2 runs");
    assert_eq!(rep.low, 2);
    assert_eq!(rep.proposals.len(), 2);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);

    // every score row carries its time; only the low ones filed a proposal
    assert_eq!(count(&conn, "SELECT count(*) FROM score WHERE ts_ms = 100"), 34);
    assert_eq!(count(&conn, "SELECT count(*) FROM score WHERE value = 0"), 2);
    assert_eq!(count(&conn, "SELECT count(*) FROM proposal"), 2);

    // the proposal of the run inside a project is low and carries all six fields
    let (loop_no, risk, project, obs, ev, mech, imp, q, rec): (i64, String, Option<i64>, String, String, String, String, String, String) = conn
        .query_row(
            "SELECT loop, risk, project_id, observation, evidence, suspected_mechanism, impact, question, recommendation \
             FROM proposal WHERE id = ?1",
            [rep.proposals[0]],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
        )
        .unwrap();
    assert_eq!((loop_no, risk.as_str(), project), (2, "low", Some(1)));
    assert!(obs.contains(&format!("run {r1}")) && obs.contains("self-acceptance"));
    assert_eq!(ev, "I reviewed my own change");
    assert!(mech.contains("separate reviewer verdict"));
    assert!(!imp.is_empty() && q.contains("What in the brief") && rec.contains("review run"));
    // a run that names no project is not inside one project: high
    let risk2: String = conn
        .query_row("SELECT risk FROM proposal WHERE id = ?1", [rep.proposals[1]], |r| r.get(0))
        .unwrap();
    assert_eq!(risk2, "high");
    let _ = r2;

    // the score row names its proposal
    let detail: String = conn
        .query_row("SELECT detail_json FROM score WHERE value = 0 AND run_id = ?1", [r1], |r| r.get(0))
        .unwrap();
    assert!(detail.contains(&format!("\"proposal_id\":{}", rep.proposals[0])));

    // a rerun scores nothing and files nothing
    let again = score_runs(&mut conn, &set, &scorer, &TemplateAgent, 200).unwrap();
    assert_eq!((again.scored, again.low), (0, 0));
    assert_eq!(count(&conn, "SELECT count(*) FROM proposal"), 2);
    assert_eq!(scorer.calls.borrow().len(), 34);

    // a 0 with no quote is not clearly present: it counts as 1 and files nothing
    let r3 = finished_run(&conn, "c", true);
    let quiet = Fake::new(&["overengineering"], None);
    let rep = score_runs(&mut conn, &set, &quiet, &TemplateAgent, 300).unwrap();
    assert_eq!((rep.scored, rep.low), (17, 0));
    let v: f64 = conn
        .query_row("SELECT value FROM score WHERE run_id = ?1 AND rubric = 'overengineering'", [r3], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 1.0);
    assert_eq!(count(&conn, "SELECT count(*) FROM proposal"), 2);

    // an owner's file replaces the set
    let custom = rubric::parse("[[rubric]]\nid = 'x'\ncriterion = 'y'\n").unwrap();
    assert_eq!(custom, vec![Rubric { id: "x".into(), criterion: "y".into() }]);
    assert!(rubric::parse("[[rubric]]\nid = 'x'\n").is_err());
    let missing = ws.root().join("none.toml");
    assert_eq!(rubric::load_or_default(&missing).unwrap().len(), 17);

    // the command runs the job once with a scorer program, no model
    let script = ws.root().join("scorer.sh");
    std::fs::write(&script, "#!/bin/sh\ncat >/dev/null\necho '{\"score\":0,\"quote\":\"q\"}'\n").unwrap();
    std::process::Command::new("chmod").arg("+x").arg(&script).status().unwrap();
    let rubrics_file = ws.root().join("r.toml");
    std::fs::write(&rubrics_file, "[[rubric]]\nid = 'one'\ncriterion = 'c'\n").unwrap();
    let before = count(&conn, "SELECT count(*) FROM proposal");
    drop(conn);
    let mut registry = Registry::new();
    let mut daemon = Daemon::new(Config { runs_dir: ws.runs_dir(), herdr: Arc::new(FakeHerdr::new()) });
    qq_core::supervisor::INSTALL(&mut registry, &mut daemon).expect("install");
    let env = Env { runs_dir: ws.runs_dir() };
    let rc = registry.dispatch(
        &env,
        &["supervisor".into(), "score".into(), "--scorer".into(), script.display().to_string(), "--rubrics".into(), rubrics_file.display().to_string()],
    );
    assert_eq!(rc, 0);
    let conn = db::open(&ws.db_path()).unwrap();
    assert_eq!(count(&conn, "SELECT count(*) FROM proposal") - before, 3, "3 runs, 1 rubric, all 0");
    assert_eq!(registry.dispatch(&env, &["supervisor".into(), "score".into()]), 2, "no scorer is refused");
}

#[test]
fn supervisor_agent_read_only() {
    use AgentAction::*;
    for a in [ReadRun, ReadFile, ReadZbrain, CodeLookup, QuestionLead, FileProposal] {
        assert!(permit(a).is_ok(), "{a:?}");
    }
    for a in [WriteFile, EditCode, WriteZbrain, SendOrder, Spawn, Merge, Push, Accept] {
        assert!(permit(a).is_err(), "{a:?}");
    }
    let tools = agent_tools();
    for t in &tools {
        assert!(!["Write", "Edit", "NotebookEdit", "Bash"].contains(&t.as_str()), "{t}");
        assert!(!t.contains("claim_") && !t.contains("campaign_begin") && !t.contains("reindex") && !t.contains("evidence_capture"), "{t}");
    }
    assert!(tools.iter().any(|t| t == "Read"));
    // no skill set (R13), so no orchestration or implementation skill either
    assert!(skills::set_for(Role::Supervisor).is_empty());

    // the brief says so too
    let f = Finding {
        run_id: 7,
        project_id: Some(1),
        rubric: Rubric { id: "self-acceptance".into(), criterion: "c".into() },
        quote: "q".into(),
    };
    let b = brief(&f);
    assert!(b.contains("read-only") && b.contains("accept nothing") && b.contains("six fields"));

    // the answer needs all six fields
    let mut a = TemplateAgent.investigate(&f).unwrap();
    assert!(a.check().is_ok());
    a.text.question = Some("  ".into());
    assert!(a.check().is_err());
    let blank = Answer { text: Text::default(), change: Change { reversible: true, projects: 1, surfaces: vec![], external_effect: false } };
    assert!(blank.check().is_err());

    // the topology agrees: a question to the Lead passes; mail and spawn do not
    let ws = TestWorkspace::new("sup-agent");
    let conn = setup(&ws);
    let cos = agent_row(&conn, "cos", "cos", None, None);
    let lead = agent_row(&conn, "lead-a", "lead", Some(1), Some(cos));
    agent_row(&conn, "p1", "peer", Some(1), Some(lead));
    agent_row(&conn, "sup", "supervisor", None, None);
    conn.execute(
        "INSERT OR IGNORE INTO topology_edge (from_role, to_role, edge) VALUES ('supervisor', 'lead', 'question')",
        [],
    )
    .unwrap();
    assert_eq!(check_question(&conn, "sup", "lead-a").unwrap(), Decision::Allowed);
    assert!(matches!(check_question(&conn, "sup", "p1").unwrap(), Decision::Refused(_)));
    assert!(matches!(check_mail(&conn, "sup", "lead-a").unwrap(), Decision::Refused(_)), "a question carries no order");
    assert!(matches!(check_spawn(&conn, "sup", Role::Peer, Some(1)).unwrap(), Decision::Refused(_)));
    assert!(!authority::refusals(&conn).unwrap().is_empty());
}

#[test]
fn loop_sensor_order() {
    // the table: every row has a computational sensor and runs it before any inferential one
    assert_eq!(LOOPS.len(), 4);
    for row in &LOOPS {
        assert!(!row.computational.is_empty(), "{}", row.name);
        assert!(!row.inferential.is_empty(), "{}", row.name);
        let kinds: Vec<Kind> = order(row).into_iter().map(|(k, _)| k).collect();
        let first_inferential = kinds.iter().position(|k| *k == Kind::Inferential).unwrap();
        assert!(kinds[first_inferential..].iter().all(|k| *k == Kind::Inferential), "{}", row.name);
        assert_eq!(first_inferential, row.computational.len(), "{}", row.name);
    }
    let outer2 = &LOOPS[2];
    assert_eq!(outer2.name, "outer 2");
    assert_eq!(outer2.computational[0], "auditor");

    // the job runs in that order, and a broken architecture rule and a failing check are findings
    let ws = TestWorkspace::new("sup-order");
    let mut conn = setup(&ws);
    finished_run(&conn, "a", true);
    let root = ws.root().join("repo");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "use std::net::TcpListener;\n").unwrap();
    let sensors = Sensors {
        root: root.clone(),
        auditor: AuditorRunOptions {
            dry_run: true,
            out: Some(ws.root().join("audit.md")),
            ..AuditorRunOptions::default()
        },
        static_checks: vec![CommandCheck {
            name: "always-red".into(),
            program: "sh".into(),
            args: vec!["-c".into(), "echo bad; exit 1".into()],
            cwd: root.clone(),
        }],
        arch_rules: vec![ArchRule {
            name: "no-listener".into(),
            dir: "src".into(),
            forbidden: vec!["TcpListener".into()],
        }],
    };
    let set = rubric::defaults();
    let scorer = Fake::new(&["polling debt"], Some("sleep 5 in a loop"));
    let rep = run_outer2(&mut conn, &sensors, &set, &scorer, &TemplateAgent, 10).unwrap();
    let want: Vec<String> = order(outer2)
        .into_iter()
        .map(|(k, n)| format!("{}:{n}", if k == Kind::Computational { "computational" } else { "inferential" }))
        .collect();
    assert_eq!(rep.trace, want);
    assert_eq!(rep.scoring.proposals.len(), 1);
    assert_eq!(rep.sensor_proposals.len(), 2, "static check and architecture rule each file one");
    let obs: Vec<String> = {
        let mut st = conn.prepare("SELECT observation FROM proposal WHERE loop = 2 ORDER BY id").unwrap();
        let v = st.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        v
    };
    assert!(obs.iter().any(|o| o == "static check 'always-red' failed"));
    assert!(obs.iter().any(|o| o == "architecture rule 'no-listener' broken"));
    // the computational findings ran before the scorer was called
    assert!(!scorer.calls.borrow().is_empty());

    // a second pass: open proposals absorb the same observations
    let again = run_outer2(&mut conn, &sensors, &set, &scorer, &TemplateAgent, 20).unwrap();
    assert!(again.sensor_proposals.is_empty());

    // once decided they are filed again, and an observation filed twice makes
    // one repeated-rule proposal, filed once ever
    conn.execute("UPDATE proposal SET status = 'rejected'", []).unwrap();
    let third = run_outer2(&mut conn, &sensors, &set, &scorer, &TemplateAgent, 30).unwrap();
    assert_eq!(third.sensor_proposals.len(), 4, "2 re-filed + 2 repeated");
    assert_eq!(count(&conn, "SELECT count(*) FROM proposal WHERE observation LIKE 'repeated:%'"), 2);
    conn.execute("UPDATE proposal SET status = 'rejected'", []).unwrap();
    let fourth = run_outer2(&mut conn, &sensors, &set, &scorer, &TemplateAgent, 40).unwrap();
    assert_eq!(fourth.sensor_proposals.len(), 2, "only the 2 observations again");
    assert_eq!(count(&conn, "SELECT count(*) FROM proposal WHERE observation LIKE 'repeated:%'"), 2);
}
