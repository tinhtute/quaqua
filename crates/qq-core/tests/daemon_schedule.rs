//! T62c: the daemon loop runs the due pulls and Supervisor jobs from schedule
//! rows; scoring is off until a scorer is configured and covers only runs that
//! ended since the last pass. The scorer is a fake: no model is called.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use qq_core::auditor::AuditorRunOptions;
use qq_core::config;
use qq_core::daemon::schedule::{self, Jobs, SENSORS_JOB};
use qq_core::daemon::{Config, Daemon};
use qq_core::db;
use qq_core::herdr::FakeHerdr;
use qq_core::source::{IdeaInbox, Source};
use qq_core::supervisor::agent::TemplateAgent;
use qq_core::supervisor::job::{
    default_arch_rules, CommandCheck, RunView, Scorer, Sensors, Verdict, SCORE_JOB,
};
use qq_core::supervisor::rubric::{self, Rubric};
use qq_core::testkit::TestWorkspace;
use rusqlite::{params, Connection};

fn template() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml");
    std::fs::read_to_string(p).expect("read template team.toml")
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Always scores 2 and records the runs it was asked about.
struct Fake(RefCell<Vec<i64>>);

impl Scorer for Fake {
    fn score(&self, _: &Rubric, run: &RunView) -> Result<Verdict, String> {
        self.0.borrow_mut().push(run.run_id);
        Ok(Verdict { value: 2, quote: None })
    }
}

fn finished_run(conn: &Connection, tag: &str, ended_ms: i64) -> i64 {
    conn.execute(
        "INSERT OR IGNORE INTO agent (name, role, started_ms) VALUES ('w', 'peer', 1)",
        [],
    )
    .unwrap();
    let kind: String = conn.query_row("SELECT name FROM kind LIMIT 1", [], |r| r.get(0)).unwrap();
    conn.execute(
        "INSERT INTO run (agent_id, kind, harness, model, started_ms, ended_ms, outcome, cost_state) \
         VALUES ((SELECT id FROM agent WHERE name = 'w'), ?1, 'claude', 'm', 1, ?2, 'done', 'unknown')",
        params![kind, ended_ms],
    )
    .unwrap();
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO run_transcript (run_id, seq, ts_ms, role, content) VALUES (?1, 0, 1, 'worker', ?2)",
        params![id, tag],
    )
    .unwrap();
    id
}

fn seeded(ws: &TestWorkspace) -> Connection {
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    config::seed(&mut conn, &template()).expect("seed template");
    conn
}

/// Sensors that touch nothing outside the workspace: dry-run auditor, one red check.
fn sensors(ws: &TestWorkspace) -> Sensors {
    let root = ws.root().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    Sensors {
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
            cwd: root,
        }],
        arch_rules: default_arch_rules(),
    }
}

fn due_at(conn: &Connection, name: &str) -> i64 {
    conn.query_row("SELECT next_due_ms FROM schedule WHERE name = ?1", [name], |r| r.get(0))
        .unwrap()
}

#[test]
fn daemon_runs_due_jobs() {
    let ws = TestWorkspace::new("sched-due");
    let mut conn = seeded(&ws);
    // a fresh database has the job rows, not yet due, and ran nothing
    for name in [SCORE_JOB, SENSORS_JOB] {
        assert!(due_at(&conn, name) > 0, "{name} has a row after migrate");
    }
    let (score_due, sensors_due) = (due_at(&conn, SCORE_JOB), due_at(&conn, SENSORS_JOB));
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM schedule WHERE last_run_ms IS NOT NULL"), 0);

    // an earlier pass left the cursor at 4: the run that ended at 5 is new
    conn.execute("UPDATE schedule SET cursor_ms = 4 WHERE name = ?1", [SCORE_JOB]).unwrap();
    finished_run(&conn, "one", 5);
    let ideas = ws.root().join("inbox/ideas");
    std::fs::create_dir_all(&ideas).unwrap();
    std::fs::write(ideas.join("dark.md"), "# Dark mode\n\nA grid theme.\n").unwrap();
    schedule::add(&conn, "idea-inbox", 1000, 100).unwrap();

    let inbox = IdeaInbox::new(ws.root());
    let srcs: [&dyn Source; 1] = [&inbox];
    let sn = sensors(&ws);
    let set = rubric::defaults();
    let fake = Fake(RefCell::new(vec![]));
    let jobs = Jobs {
        sources: &srcs,
        sensors: &sn,
        rubrics: &set,
        scorer: Some(&fake),
        agent: &TemplateAgent,
    };

    // before anything is due the pass runs nothing
    let ran = schedule::run_due(&mut conn, &jobs, 50).unwrap();
    assert!(ran.jobs.is_empty() && ran.proposals.is_empty() && ran.errors.is_empty());
    assert!(fake.0.borrow().is_empty());

    // the pull is due: the idea is filed, the jobs still wait
    let ran = schedule::run_due(&mut conn, &jobs, 100).unwrap();
    assert_eq!(ran.proposals.len(), 1);
    assert!(ran.jobs.is_empty());

    // the job rows are due: scoring runs first, then the sensors
    let ran = schedule::run_due(&mut conn, &jobs, sensors_due.max(score_due)).unwrap();
    assert_eq!(ran.jobs, [SCORE_JOB, SENSORS_JOB]);
    assert!(!fake.0.borrow().is_empty(), "scorer was called");
    assert!(!ran.proposals.is_empty(), "the red check is filed");
    // each row moved one interval and nothing is due again at once
    let interval: i64 = conn
        .query_row("SELECT interval_ms FROM schedule WHERE name = ?1", [SENSORS_JOB], |r| r.get(0))
        .unwrap();
    assert_eq!(due_at(&conn, SENSORS_JOB), sensors_due.max(score_due) + interval);
    let again = schedule::run_due(&mut conn, &jobs, sensors_due.max(score_due)).unwrap();
    assert!(again.jobs.is_empty() && again.proposals.is_empty());
    drop(conn);

    // the real daemon loop: it adds the source rows, then pulls the one made due
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: Arc::new(FakeHerdr::new()),
    });
    d.register_verb("noop", Box::new(|_, _| Ok(serde_json::Value::Null))).unwrap();
    let handle = d.spawn().expect("daemon starts");
    let conn = db::open(&ws.db_path()).unwrap();
    std::fs::write(ideas.join("light.md"), "# Light mode\n\nAnother theme.\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if count(&conn, "SELECT COUNT(*) FROM schedule WHERE name = 'idea-inbox'") == 1 {
            conn.execute("UPDATE schedule SET next_due_ms = 0 WHERE name = 'idea-inbox'", []).unwrap();
            break;
        }
        assert!(Instant::now() < deadline, "the daemon never added the source rows");
        std::thread::sleep(Duration::from_millis(10));
    }
    let before = count(&conn, "SELECT COUNT(*) FROM proposal WHERE loop = 3");
    loop {
        if count(&conn, "SELECT COUNT(*) FROM proposal WHERE loop = 3") > before {
            break;
        }
        assert!(Instant::now() < deadline, "the daemon loop never pulled the due source");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM schedule WHERE name = 'farm-failures'"), 1);
    handle.stop().unwrap();
}

#[test]
fn scoring_off_without_scorer() {
    let ws = TestWorkspace::new("sched-off");
    let mut conn = seeded(&ws);
    // unset is the shipped default; an empty value is unset too
    assert!(schedule::configured_scorer(&conn).is_none());
    conn.execute("INSERT INTO setting (key, value_json) VALUES ('limits.supervisor_scorer', '\"\"')", []).unwrap();
    assert!(schedule::configured_scorer(&conn).is_none());
    conn.execute("UPDATE setting SET value_json = '\"/bin/echo\"' WHERE key = 'limits.supervisor_scorer'", []).unwrap();
    assert!(schedule::configured_scorer(&conn).is_some());
    conn.execute("DELETE FROM setting WHERE key = 'limits.supervisor_scorer'", []).unwrap();

    finished_run(&conn, "old", 5);
    conn.execute("UPDATE schedule SET next_due_ms = 0 WHERE name = ?1", [SCORE_JOB]).unwrap();
    let sn = sensors(&ws);
    let set = rubric::defaults();
    let jobs = Jobs {
        sources: &[],
        sensors: &sn,
        rubrics: &set,
        scorer: None,
        agent: &TemplateAgent,
    };
    let ran = schedule::run_due(&mut conn, &jobs, 10).unwrap();
    assert!(!ran.jobs.contains(&SCORE_JOB), "scoring did nothing");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM score"), 0);
    let cursor: Option<i64> = conn
        .query_row("SELECT cursor_ms FROM schedule WHERE name = ?1", [SCORE_JOB], |r| r.get(0))
        .unwrap();
    assert_eq!(cursor, None, "the cursor did not move");

    // the setting is reachable through team.toml and survives export and diff
    let mut conn = db::open(&ws.root().join("runs/second.db")).unwrap();
    let text = template().replacen("[limits]\n", "[limits]\nsupervisor_scorer = \"/bin/echo\"\n", 1);
    config::seed(&mut conn, &text).unwrap();
    assert!(schedule::configured_scorer(&conn).is_some());
    assert!(config::export_toml(&conn).unwrap().contains("supervisor_scorer"));
    config::diff(&conn, &text).unwrap();
}

#[test]
fn scoring_cursor_new_runs_only() {
    let ws = TestWorkspace::new("sched-cursor");
    // the migration starts the cursor at the newest run that already ended
    {
        let mut raw = Connection::open_in_memory().unwrap();
        raw.pragma_update(None, "foreign_keys", false).unwrap();
        for (v, sql) in db::migrate::LADDER.iter().filter(|(v, _)| *v < 10) {
            raw.execute_batch(sql).unwrap();
            raw.pragma_update(None, "user_version", *v).unwrap();
        }
        raw.execute(
            "INSERT INTO run (agent_id, kind, harness, model, started_ms, ended_ms, cost_state) \
             VALUES (1, 'k', 'h', 'm', 1, 77, 'unknown')",
            [],
        )
        .unwrap();
        db::migrate::migrate(&mut raw).unwrap();
        let c: i64 = raw
            .query_row("SELECT cursor_ms FROM schedule WHERE name = ?1", [SCORE_JOB], |r| r.get(0))
            .unwrap();
        assert_eq!(c, 77, "history before the migration is not scored");
    }

    let mut conn = seeded(&ws);
    // runs that finish before the first pass are history: the first pass starts the cursor, scores nothing
    finished_run(&conn, "a", 10);
    finished_run(&conn, "b", 20);
    conn.execute("UPDATE schedule SET next_due_ms = 0 WHERE name = ?1", [SCORE_JOB]).unwrap();
    let sn = sensors(&ws);
    let set = rubric::defaults();
    let fake = Fake(RefCell::new(vec![]));
    let jobs = Jobs {
        sources: &[],
        sensors: &sn,
        rubrics: &set,
        scorer: Some(&fake),
        agent: &TemplateAgent,
    };
    let seen = |f: &Fake| {
        let mut v = f.0.borrow().clone();
        v.sort();
        v.dedup();
        v
    };
    let cursor = |c: &Connection| -> i64 {
        c.query_row("SELECT cursor_ms FROM schedule WHERE name = ?1", [SCORE_JOB], |r| r.get(0)).unwrap()
    };

    assert_eq!(count(&conn, "SELECT COUNT(*) FROM schedule WHERE name = 'supervisor.score' AND cursor_ms IS NULL"), 1);
    schedule::run_due(&mut conn, &jobs, 100).unwrap();
    assert!(fake.0.borrow().is_empty(), "no history is scored");
    assert_eq!(cursor(&conn), 20);

    // a pass with nothing new asks the scorer nothing
    fake.0.borrow_mut().clear();
    conn.execute("UPDATE schedule SET next_due_ms = 0 WHERE name = ?1", [SCORE_JOB]).unwrap();
    schedule::run_due(&mut conn, &jobs, 200).unwrap();
    assert!(fake.0.borrow().is_empty());

    // only the run that ended after the cursor is scored next
    let r3 = finished_run(&conn, "c", 30);
    conn.execute("UPDATE schedule SET next_due_ms = 0 WHERE name = ?1", [SCORE_JOB]).unwrap();
    schedule::run_due(&mut conn, &jobs, 300).unwrap();
    assert_eq!(seen(&fake), [r3]);
    assert_eq!(cursor(&conn), 30);
}

struct Broken;

impl Scorer for Broken {
    fn score(&self, _: &Rubric, _: &RunView) -> Result<Verdict, String> {
        Err("scorer exploded".into())
    }
}

#[test]
fn scorer_errors_come_back() {
    let ws = TestWorkspace::new("sched-errors");
    let mut conn = seeded(&ws);
    finished_run(&conn, "a", 10);
    conn.execute("UPDATE schedule SET cursor_ms = 5, next_due_ms = 0 WHERE name = ?1", [SCORE_JOB]).unwrap();
    let sn = sensors(&ws);
    let set = rubric::defaults();
    let jobs = Jobs {
        sources: &[],
        sensors: &sn,
        rubrics: &set,
        scorer: Some(&Broken),
        agent: &TemplateAgent,
    };
    let ran = schedule::run_due(&mut conn, &jobs, 100).unwrap();
    assert!(
        ran.errors.iter().any(|e| e.contains("scorer exploded")),
        "the scorer error is reported: {:?}",
        ran.errors
    );
}

#[test]
fn bad_rubrics_file_stops_scoring_only() {
    let ws = TestWorkspace::new("sched-badrubric");
    let conn = seeded(&ws);
    let runs = ws.runs_dir();
    let rp = rubric::rubrics_path(&runs);
    std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
    std::fs::write(&rp, "this is not [[ toml").unwrap();
    let inbox = runs.parent().unwrap().join("inbox/ideas");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("a.md"), "An idea\n").unwrap();
    conn.execute("INSERT INTO setting (key, value_json) VALUES ('limits.supervisor_scorer', '\"/bin/true\"')", []).unwrap();
    let now = 1_000;
    schedule::ensure_sources(&conn, 0).unwrap();
    conn.execute("UPDATE schedule SET next_due_ms = 0", []).unwrap();
    let before = count(&conn, "SELECT COUNT(*) FROM proposal");
    let errors = schedule::tick_at(&runs, now);
    assert!(errors.iter().any(|e| e.contains("rubrics")), "reported: {errors:?}");
    assert!(count(&conn, "SELECT COUNT(*) FROM proposal") > before, "the idea was still pulled");
    let due = count(&conn, &format!("SELECT COUNT(*) FROM schedule WHERE next_due_ms <= {now}"));
    assert_eq!(due, 0, "every row moved, nothing spins");
}
