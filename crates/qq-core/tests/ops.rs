//! Worker operations and farm views: brief check and watchdog decisions,
//! cleanup/stop, four-per-tab placement, status from the database. Agents run
//! on FakeHerdr; pane operations run on a test-local in-memory host because the
//! frozen herdr seam has no pane verbs yet.

use std::path::Path;
use std::process::Command;

use qq_core::config;
use qq_core::db;
use qq_core::driver::Registry;
use qq_core::grid::{self, HerdrPanes, PaneOps};
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::status;
use qq_core::task::cleanup::{self, OpsError};
use qq_core::task::prompt::{self, Allowance, Clock, Exit, Observed, WaitCfg};
use qq_core::task::spawn::{spawn, Role, SpawnSpec};
use qq_core::task::worker_flow::place_pane;
use rusqlite::Connection;

const BRIEF: &str = "Topology: solo — one unit\nPlaybook: feature\nContext: none, test\n\
Goal: g\nHard constraints: h\nCurrent design (open to challenge): d\n\
Unknowns: u\nEvidence: e\nTime budget: 15 min\nCost budget: $2\n";

fn seeded(tag: &str) -> (std::path::PathBuf, Connection) {
    let dir = std::env::temp_dir().join(format!("qq-ops-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let conn = db::open(&dir.join(db::path::DB_FILE)).unwrap();
    let team = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/workspace/team.toml"),
    )
    .unwrap()
    .replace("harness = \"omp\"", "harness = \"opencode\"");
    let mut c = conn;
    config::seed(&mut c, &team).unwrap();
    (dir, c)
}

fn add_agent(conn: &Connection, name: &str, role: &str, kind: &str, project: Option<i64>) {
    conn.execute(
        "INSERT INTO agent (name, role, kind, project_id, started_ms) VALUES (?1, ?2, ?3, ?4, 1)",
        rusqlite::params![name, role, kind, project],
    )
    .unwrap();
}

fn add_project(conn: &Connection, slug: &str) -> i64 {
    conn.execute(
        "INSERT INTO project (slug, created_ms) VALUES (?1, 1)",
        [slug],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn ended(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT ended_ms IS NOT NULL FROM agent WHERE name = ?1",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

/// Pane helpers over FakeHerdr.
fn occupy(h: &FakeHerdr, pane: &str, agent: &str) {
    let r = h.agent_start(agent, "pi", pane, 1, &[]).unwrap();
    assert!(r.ok, "{}", r.body);
}

fn tabs(h: &FakeHerdr) -> Vec<String> {
    let mut t: Vec<String> = HerdrPanes(h)
        .panes()
        .unwrap()
        .into_iter()
        .map(|p| p.tab)
        .collect();
    t.sort();
    t.dedup();
    t
}

#[test]
fn prompt_check_brief_and_resume() {
    let ok = prompt::check_brief(BRIEF, None).unwrap();
    assert_eq!((ok.budget_sec, ok.source), (900, "brief"));
    assert!(ok.warnings.is_empty());
    // the env override wins, no budget line falls to the default
    assert_eq!(prompt::check_brief(BRIEF, Some(60)).unwrap().source, "env");
    let no_time = BRIEF.replace("Time budget: 15 min\n", "");
    let d = prompt::check_brief(&no_time, None).unwrap();
    assert_eq!((d.budget_sec, d.source), (2700, "default"));
    // rejected briefs
    for bad in [
        BRIEF.replace("Topology: solo — one unit\n", ""),
        BRIEF.replace("Goal: g\n", ""),
        format!("{BRIEF}Must fix: urgent\n"),
        format!("{BRIEF}Review rounds: 0\n"),
        BRIEF.replace("15 min", "soon"),
    ] {
        assert!(prompt::check_brief(&bad, None).is_err(), "{bad}");
    }
    // advisory gaps only warn
    let w =
        prompt::check_brief(&BRIEF.replace("Playbook: feature", "Playbook: none"), None).unwrap();
    assert_eq!(w.warnings.len(), 1);

    // watchdog codes
    let base = Observed {
        budget_sec: 900,
        ..Default::default()
    };
    assert_eq!(prompt::decide(&base), None);
    let code = |o: Observed| prompt::decide(&o).map(Exit::code);
    assert_eq!(
        code(Observed {
            report: Some("Outcome: COMPLETE\n## Lessons\nnone\n".into()),
            ..base.clone()
        }),
        Some(0)
    );
    assert_eq!(
        code(Observed {
            report: Some("Outcome: COMPLETE\n".into()),
            ..base.clone()
        }),
        Some(6)
    );
    assert_eq!(
        code(Observed {
            elapsed_sec: 7200,
            ..base.clone()
        }),
        Some(2)
    );
    assert_eq!(
        code(Observed {
            since_change_sec: 600,
            ..base.clone()
        }),
        Some(3)
    );
    assert_eq!(
        code(Observed {
            idle_sec: 120,
            ..base.clone()
        }),
        Some(3)
    );
    assert_eq!(
        code(Observed {
            elapsed_sec: 900,
            ..base.clone()
        }),
        Some(4)
    );
    assert_eq!(
        code(Observed {
            blocked: true,
            ..base.clone()
        }),
        Some(5)
    );
    assert_eq!(
        code(Observed {
            blocked: true,
            provider_error: true,
            ..base.clone()
        }),
        Some(7)
    );
    assert_eq!(
        code(Observed {
            core_died: true,
            ..base.clone()
        }),
        Some(8)
    );
    // a worker that is not blocked and not stalled is never a provider error
    assert_eq!(
        code(Observed {
            provider_error: true,
            ..base.clone()
        }),
        None
    );

    // resume: the same observation under an extended allowance keeps waiting
    let over = Observed {
        elapsed_sec: 950,
        budget_sec: 900,
        ..Default::default()
    };
    assert_eq!(code(over.clone()), Some(4));
    let a = Allowance(900).extend(300);
    assert_eq!(
        prompt::decide(&Observed {
            budget_sec: a.0,
            ..over
        }),
        None
    );
    assert!(prompt::parse_extend("0").is_err());
    assert_eq!(prompt::parse_extend("90"), Ok(90));
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@local")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@local")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn cleanup_keeps_branch() {
    let (dir, conn) = seeded("cleanup");
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a"), "a").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let wt = dir.join("wt-w1");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "quaqua/w1",
            &wt.to_string_lossy(),
        ],
    );
    std::fs::write(wt.join("b"), "b").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    add_agent(&conn, "w1", "peer", "tieude", None);

    cleanup::cleanup_worker(&conn, &repo, &wt, "w1", 5).unwrap();
    assert!(!wt.exists());
    assert!(git(&repo, &["branch", "--list", "quaqua/w1"]).contains("quaqua/w1"));
    assert!(ended(&conn, "w1"));
    // a repeat converges
    cleanup::cleanup_worker(&conn, &repo, &wt, "w1", 6).unwrap();
}

#[test]
fn stop_refuses_unverified_harness_and_releases_pane_when_idle() {
    let (_dir, conn) = seeded("stop");
    let herdr = FakeHerdr::new();
    let panes = HerdrPanes(&herdr);
    let drivers = Registry::native();
    // claude has no verified stop sequence: a working agent is refused, pane kept
    add_agent(&conn, "c1", "peer", "ni", None);
    let first = panes.create_tab("r-workers").unwrap();
    herdr
        .agent_start("c1", "claude", &first.id, 1, &[])
        .unwrap();
    herdr.set_status("c1", "working");
    let r = cleanup::stop_worker(&conn, &herdr, &panes, &drivers, "c1", 9);
    assert_eq!(r, Err(OpsError::NotStopped("c1".into())));
    assert!(
        herdr.keys().is_empty(),
        "nothing is sent for an unverified harness"
    );
    assert!(!ended(&conn, "c1"));
    assert_eq!(panes.panes().unwrap().len(), 1);

    // pi stops on esc: pane released, agent ended
    add_agent(&conn, "p1", "peer", "tieude", None);
    let second = panes.split(&first.id).unwrap();
    herdr.agent_start("p1", "pi", &second.id, 1, &[]).unwrap();
    herdr.set_status("p1", "working");
    herdr.idle_on_keys("p1", &["esc"]);
    cleanup::stop_worker(&conn, &herdr, &panes, &drivers, "p1", 9).unwrap();
    assert!(ended(&conn, "p1"));
    let left = panes.panes().unwrap();
    assert_eq!(
        left.len(),
        1,
        "the stopped worker's pane is closed: {left:?}"
    );
    assert_eq!(left[0].agent.as_deref(), Some("c1"));
}

#[test]
fn grid_four_per_tab() {
    let herdr = FakeHerdr::new();
    let panes = HerdrPanes(&herdr);
    let mut got = Vec::new();
    // five spawns with no pane named: each lands where grid placement says
    let (_sd, sconn) = seeded("spawnplace");
    sconn
        .execute(
            "INSERT INTO project (slug, repo_path, created_ms) VALUES ('repo', '/x/repo', 1)",
            [],
        )
        .unwrap();
    let drivers = Registry::native();
    for i in 0..5 {
        let name = format!("w{i}");
        let id = place_pane(&sconn, &herdr, &name, Some(1)).unwrap();
        spawn(
            &sconn,
            &herdr,
            &drivers,
            &SpawnSpec {
                name: &name,
                kind: "tieude",
                role: Role::Peer,
                pane: &id,
                project_id: Some(1),
                spawner_id: None,
            },
            2,
        )
        .unwrap();
        got.push(id);
    }
    // a repeat of a spawn keeps its pane rather than placing a new one
    assert_eq!(place_pane(&sconn, &herdr, "w2", Some(1)).unwrap(), got[2]);
    assert_eq!(tabs(&herdr), vec!["repo-workers", "repo-workers-2"]);
    let per_tab = |t: &str| panes.panes().unwrap().iter().filter(|p| p.tab == t).count();
    assert_eq!((per_tab("repo-workers"), per_tab("repo-workers-2")), (4, 1));
    // a closed pane's slot is refilled before a third tab opens
    panes.close_pane(&got[1]).unwrap();
    let id = grid::acquire(&panes, "repo").unwrap();
    let tab = panes
        .panes()
        .unwrap()
        .into_iter()
        .find(|p| p.id == id)
        .unwrap()
        .tab;
    assert_eq!(tab, "repo-workers");
    assert_eq!(grid::place("repo", &[4, 4]).tab, "repo-workers-3");

    // reconcile closes the pane of an agent the DB ended, and only that one
    let (_d, conn) = seeded("grid");
    for n in ["w0", "w2", "w3", "w4"] {
        add_agent(&conn, n, "peer", "tieude", None);
    }
    conn.execute("UPDATE agent SET ended_ms = 2 WHERE name = 'w0'", [])
        .unwrap();
    let ops = grid::reconcile(&panes, &conn).unwrap();
    assert_eq!(ops, vec![grid::Op::Close(got[0].clone())]);
    assert!(grid::reconcile(&panes, &conn).unwrap().is_empty());
}

#[test]
fn status_reads_db() {
    let (_d, conn) = seeded("status");
    let pid = add_project(&conn, "zimage");
    add_agent(&conn, "lead1", "lead", "ni", Some(pid));
    add_agent(&conn, "w1", "peer", "tieude", Some(pid));
    conn.execute("UPDATE agent SET ended_ms = 3 WHERE name = 'w1'", [])
        .unwrap();
    conn.execute(
        "INSERT INTO run (agent_id, kind, harness, model, started_ms, ended_ms, outcome, tokens_in, tokens_out, cost_usd, cost_state) \
         SELECT id, 'tieude', 'pi', 'm', 1, 2, 'verified', 10, 20, NULL, 'unknown' FROM agent WHERE name = 'w1'",
        [],
    )
    .unwrap();
    let open = status::render(&status::read(&conn, false).unwrap(), false);
    assert!(open.contains("lead1 lead ni zimage"));
    assert!(
        !open.contains("w1"),
        "ended agents hide without --all: {open}"
    );
    let all = status::render(&status::read(&conn, true).unwrap(), true);
    assert!(all.contains("w1 peer tieude zimage (ended)"), "{all}");
    assert!(
        all.contains("cost=unknown"),
        "an unreported cost is never 0: {all}"
    );
    assert!(all.contains("in=10 out=20"), "{all}");
}

#[test]
fn pane_metadata_merges_and_repeats() {
    let herdr = FakeHerdr::new();
    let pane = HerdrPanes(&herdr).create_tab("t").unwrap().id;
    let tokens = |r: qq_core::herdr::Reply| {
        let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
        v.pointer("/result/pane/tokens").unwrap().clone()
    };
    herdr
        .pane_report_metadata(&pane, "s", &[("grid_role", "lead")], Some("L"))
        .unwrap();
    herdr
        .pane_report_metadata(&pane, "s", &[("grid_farm", "f")], None)
        .unwrap();
    herdr
        .pane_report_metadata(&pane, "s", &[("grid_farm", "f")], None)
        .unwrap();
    let got = herdr.pane_get(&pane).unwrap();
    assert_eq!(
        tokens(got.clone()),
        serde_json::json!({"grid_role": "lead", "grid_farm": "f"})
    );
    assert!(got.body.contains("\"display_agent\":\"L\""));
    assert!(!herdr.pane_get("nope").unwrap().ok);
}

/// A clock the test owns: sleeping advances it and runs `on_sleep(now)`.
struct FakeClock<'a> {
    now: std::cell::Cell<u64>,
    on_sleep: Box<dyn Fn(u64) + 'a>,
}

impl Clock for FakeClock<'_> {
    fn now_sec(&self) -> u64 {
        self.now.get()
    }
    fn sleep_ms(&self, ms: u64) {
        self.now.set(self.now.get() + ms / 1000);
        (self.on_sleep)(self.now.get());
    }
}

fn wait_cfg<'a>(
    runs: &'a Path,
    journal: &'a dyn Fn(&str, serde_json::Value),
    budget_sec: u64,
) -> WaitCfg<'a> {
    WaitCfg {
        name: "w",
        runs_dir: runs,
        report: runs.join("w.md"),
        worktree: None,
        budget_sec,
        poll_ms: 5_000,
        retry_base_ms: 60_000,
        retry_max: 4,
        journal,
    }
}

#[test]
fn prompt_wait_exits_on_report_and_on_timebox() {
    let (runs, _c) = seeded("wait");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "working");
    let journal = |_: &str, _: serde_json::Value| {};

    // A report appears at t=20: exit 0, and no nudge was due (budget 100).
    let r = runs.join("w.md");
    let clock = FakeClock {
        now: 1000.into(),
        on_sleep: Box::new(|now| {
            if now >= 1020 {
                std::fs::write(
                    &r,
                    "Outcome: COMPLETE\n\n## Blockers\nnone\n\n## Lessons\nnone\n",
                )
                .unwrap();
            }
        }),
    };
    let cfg = wait_cfg(&runs, &journal, 100);
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::ReportReady);
    assert!(h.prompts().is_empty());
    std::fs::remove_file(&r).unwrap();
    for f in [".w.budget", ".w.elapsed", ".w.nudged"] {
        let _ = std::fs::remove_file(runs.join(f));
    }

    // Silence past the budget: one 80% nudge, then exit 4.
    let clock = FakeClock {
        now: 5000.into(),
        on_sleep: Box::new(|_| {}),
    };
    let cfg = wait_cfg(&runs, &journal, 60);
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::BudgetExceeded);
    assert_eq!(clock.now_sec() - 5000, 60);
    let nudges: Vec<_> = h
        .prompts()
        .into_iter()
        .filter(|(_, t)| t.starts_with("Time check"))
        .collect();
    assert_eq!(nudges.len(), 1);
    assert_eq!(
        std::fs::read_to_string(runs.join(".w.budget"))
            .unwrap()
            .trim(),
        "60"
    );
    assert!(runs.join(".w.nudged").exists());
}

#[test]
fn prompt_wait_retries_a_provider_error_then_escalates() {
    let (runs, _c) = seeded("retry");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    let events = std::cell::RefCell::new(Vec::<String>::new());
    let journal = |k: &str, _: serde_json::Value| events.borrow_mut().push(k.to_string());
    let err = runs.join(".w.error");
    std::fs::write(&err, "overloaded\n").unwrap();
    let again = err.clone();
    // Each retry clears the verdict; the harness errors again after a sleep.
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(move |_| {
            let _ = std::fs::write(&again, "overloaded\n");
        }),
    };
    let cfg = wait_cfg(&runs, &journal, 7200);
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::ProviderError);
    let ev = events.borrow();
    assert_eq!(ev.iter().filter(|k| *k == "provider_retry").count(), 4);
    assert_eq!(ev.last().map(String::as_str), Some("provider_escalate"));
    assert_eq!(
        h.prompts().iter().filter(|(_, t)| t == "continue").count(),
        4
    );
}
