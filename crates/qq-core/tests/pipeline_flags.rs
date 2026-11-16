//! `pipeline` plan-ready pause, `--resume` and `--rounds`: what the Lead sees
//! through the flow's prompts, pauses and the verb. Agents run on FakeHerdr.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qq_core::cli::{Env, Registry as CliRegistry};
use qq_core::config;
use qq_core::daemon::{Config, Daemon};
use qq_core::db;
use qq_core::driver::{HarnessDriver, Registry};
use qq_core::engine::activity::HerdrRequest;
use qq_core::engine::journal::Journal;
use qq_core::engine::runner::{run_until_blocked, RunnerOptions, RunnerOutcome};
use qq_core::engine::{
    Activity, ActivityError, ActivityRequest, ActivityResult, InMemoryJournal, VirtualClock,
    WorkflowEvent,
};
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::rpc::{self, Request};
use qq_core::task::pipeline::{PipelineFlow, PipelineInput, PipelineOutput, PipelineState};
use qq_core::task::pipeline_io::{self, Deps, Effects};
use qq_core::testkit::TestWorkspace;
use serde_json::{json, Value};

const WF: &str = "pipeline:t1";

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
        .expect("git runs");
    assert!(o.status.success(), "git {args:?}");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn seed(conn: &mut rusqlite::Connection) {
    let template =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/workspace/team.toml");
    let team = std::fs::read_to_string(template)
        .unwrap()
        .replace("harness = \"omp\"", "harness = \"opencode\"");
    config::seed(conn, &team).expect("seed");
}

struct Opencode;

impl HarnessDriver for Opencode {
    fn harness(&self) -> &str {
        "opencode"
    }
    fn herdr_kind(&self) -> &str {
        "opencode"
    }
    fn launch_args(&self, model: &str, _effort: Option<&str>) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }
}

fn drivers() -> Registry {
    let mut r = Registry::native();
    r.register(Opencode);
    r
}

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    runs: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("qq-pflags-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let runs = root.join("runs");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&runs).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        git(&repo, &["branch", "integ"]);
        git(&repo, &["branch", "t1"]);
        let mut conn = db::open(&runs.join(db::path::DB_FILE)).expect("open qq db");
        seed(&mut conn);
        Fixture { root, repo, runs }
    }

    fn commit_on_t1(&self, file: &str) -> String {
        let wt = self.root.join("wt");
        if !wt.exists() {
            git(
                &self.repo,
                &["worktree", "add", "-q", &wt.to_string_lossy(), "t1"],
            );
        }
        std::fs::write(wt.join(file), format!("{file}\n")).unwrap();
        git(&wt, &["add", "."]);
        git(&wt, &["commit", "-q", "-m", file]);
        git(&wt, &["rev-parse", "HEAD"])
    }

    fn input(&self, plan: bool, max_rounds: u32) -> PipelineInput {
        serde_json::from_value(json!({
            "task": "t1", "builder_kind": "ni", "reviewer_kind": "sude",
            "brief": "Goal: x\n", "repo": self.repo.to_string_lossy(),
            "branch": "t1", "integ": "integ",
            "runs_dir": self.runs.to_string_lossy(), "pane": "p1",
            "plan": plan, "max_rounds": max_rounds,
        }))
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The production effects over a fake herdr; records prompts and watchers.
struct Sim {
    eff: Effects,
    armed: Arc<Mutex<Vec<String>>>,
    prompts: Mutex<Vec<(String, String)>>,
}

impl Activity for Sim {
    fn execute(&self, req: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        if let ActivityRequest::Herdr(HerdrRequest::Prompt { name, task }) = req {
            self.prompts
                .lock()
                .unwrap()
                .push((name.clone(), task.clone()));
        }
        if let ActivityRequest::Herdr(HerdrRequest::Spawn { name, .. }) = req {
            // herdr hosts one agent per pane: the last round's reviewer has exited
            if let Some((task, n)) = name.rsplit_once("-r") {
                if let Ok(n) = n.parse::<u32>() {
                    self.eff_herdr_remove(&format!("{task}-r{}", n - 1));
                }
            }
        }
        self.eff.execute(req)
    }
}

thread_local! {
    static HERDR: std::cell::RefCell<Option<Arc<FakeHerdr>>> = const { std::cell::RefCell::new(None) };
}

impl Sim {
    fn eff_herdr_remove(&self, name: &str) {
        HERDR.with(|h| {
            if let Some(h) = h.borrow().as_ref() {
                h.remove(name);
            }
        });
    }

    fn builder_prompts(&self) -> Vec<String> {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .filter(|(to, _)| to == "t1-b")
            .map(|(_, t)| t.clone())
            .collect()
    }
}

struct Run {
    sim: Sim,
    journal: InMemoryJournal,
    clock: VirtualClock,
}

fn hold() -> RunnerOptions {
    RunnerOptions {
        advance_virtual_time: false,
        ..RunnerOptions::default()
    }
}

impl Run {
    fn start(fx: &Fixture, input: PipelineInput) -> Self {
        let herdr = Arc::new(FakeHerdr::new());
        herdr
            .agent_start(&input.builder(), "claude", "%1", 1, &[])
            .unwrap();
        HERDR.with(|h| *h.borrow_mut() = Some(herdr.clone()));
        let armed = Arc::new(Mutex::new(Vec::new()));
        let a = armed.clone();
        let deps = Deps {
            herdr,
            runs_dir: fx.runs.clone(),
            drivers: drivers(),
            watch: Arc::new(move |_, path| {
                a.lock().unwrap().push(path.to_string());
                Ok(())
            }),
        };
        let mut r = Run {
            sim: Sim {
                eff: Effects::new(Arc::new(deps), input.clone()),
                armed,
                prompts: Mutex::new(Vec::new()),
            },
            journal: InMemoryJournal::new(),
            clock: VirtualClock::new(1_000_000),
        };
        r.run(Some(PipelineState::new(input)), Vec::new());
        r.signal("pipeline", "start", Value::Null);
        r
    }

    fn run(
        &mut self,
        state: Option<PipelineState>,
        events: Vec<WorkflowEvent>,
    ) -> RunnerOutcome<PipelineOutput> {
        run_until_blocked(
            &PipelineFlow,
            WF,
            state,
            &mut self.journal,
            &mut self.sim,
            &mut self.clock,
            &hold(),
            events,
        )
        .unwrap()
    }

    fn signal(
        &mut self,
        source: &str,
        name: &str,
        payload: Value,
    ) -> RunnerOutcome<PipelineOutput> {
        let ev = WorkflowEvent::Signal {
            source: source.into(),
            name: name.into(),
            payload,
        };
        self.run(None, vec![ev])
    }

    /// The worker writes the report the flow waits for; the watcher reports it.
    fn answer(&mut self, text: &str) -> RunnerOutcome<PipelineOutput> {
        let path = self.sim.armed.lock().unwrap().last().unwrap().clone();
        std::fs::write(&path, text).unwrap();
        self.signal("report", "appeared", json!({ "path": path }))
    }

    /// (code, reason) of the pause the flow sits in.
    fn blocked(&self) -> Option<(i64, String)> {
        let snap = self.journal.load_latest_snapshot(WF).unwrap().unwrap();
        let v: Value = serde_json::from_str(&snap.state_json).unwrap();
        let b = v.get("blocked").filter(|b| !b.is_null())?;
        Some((
            b["code"].as_i64().unwrap(),
            b["reason"].as_str().unwrap().to_string(),
        ))
    }
}

fn build_report(sha: &str) -> String {
    format!("Outcome: COMPLETE\nVerified: unit-test-verified {sha}\n\n## Blockers\nnone\n\n## Lessons\nnone\n")
}

const PLAN: &str = "## Plan\n1. do x\n\n## Blockers\nnone\n\n## Lessons\nnone\n";
const FINDING: &str = "## Findings\n- [major] x\nVerdict: 1 findings\n";

#[test]
fn a_planning_pipeline_pauses_plan_ready_and_builds_only_after_resume() {
    let fx = Fixture::new("plan");
    let mut r = Run::start(&fx, fx.input(true, 2));
    let first = r.sim.builder_prompts();
    assert_eq!(first.len(), 1);
    assert!(first[0].contains("PLAN"), "{}", first[0]);
    assert!(!first[0].contains("BUILD"), "{}", first[0]);

    let out = r.answer(PLAN);
    assert!(matches!(out, RunnerOutcome::Blocked(_)), "{out:?}");
    let (code, reason) = r.blocked().expect("paused");
    assert_eq!(code, 5, "plan-ready is exit 5");
    assert!(reason.contains("plan-ready"), "{reason}");
    assert!(reason.contains("pipeline --resume t1"), "{reason}");
    assert_eq!(r.sim.builder_prompts().len(), 1, "no build before approval");

    r.signal("pipeline", "resume", Value::Null);
    let all = r.sim.builder_prompts();
    assert_eq!(all.len(), 2);
    assert!(all[1].contains("BUILD"), "{}", all[1]);
    assert!(r.blocked().is_none() || r.blocked().unwrap().0 != 5);

    assert!(all[1].contains("Follow the approved plan"), "{}", all[1]);

    // a resume of a flow that is not paused starts nothing
    r.signal("pipeline", "resume", Value::Null);
    assert_eq!(r.sim.builder_prompts().len(), 2);
}

#[test]
fn a_pipeline_without_plan_builds_at_once() {
    let fx = Fixture::new("noplan");
    let r = Run::start(&fx, fx.input(false, 2));
    let all = r.sim.builder_prompts();
    assert_eq!(all.len(), 1);
    assert!(all[0].contains("BUILD"), "{}", all[0]);
}

/// Drives a flow with a cap of one round to its rounds-exhausted pause, then
/// resumes with `payload`; returns the run after the second review failed.
fn exhausted_then_resumed(fx: &Fixture, payload: Value) -> Run {
    let mut r = Run::start(fx, fx.input(false, 1));
    r.answer(&build_report(&fx.commit_on_t1("a.txt")));
    r.answer(FINDING);
    let (code, reason) = r.blocked().expect("exhausted");
    assert_eq!(code, 5);
    assert!(reason.contains("exhausted"), "{reason}");
    assert_eq!(r.sim.builder_prompts().len(), 1, "no fix round is free");
    r.signal("pipeline", "resume", payload);
    assert_eq!(
        r.sim.builder_prompts().len(),
        2,
        "the resume starts the fix"
    );
    r.answer(&build_report(&fx.commit_on_t1("b.txt")));
    r.answer(FINDING);
    r
}

#[test]
fn resume_without_rounds_grants_one_more_round_then_pauses_again() {
    let fx = Fixture::new("r-plain");
    let r = exhausted_then_resumed(&fx, Value::Null);
    let (_, reason) = r.blocked().expect("round 2 is the last");
    assert!(reason.contains("exhausted"), "{reason}");
    assert_eq!(r.sim.builder_prompts().len(), 2, "no further fix round");
}

#[test]
fn resume_rounds_raises_the_cap_so_the_next_fix_needs_no_approval() {
    let fx = Fixture::new("r-cap");
    let r = exhausted_then_resumed(&fx, json!({ "rounds": 3 }));
    assert_eq!(r.sim.builder_prompts().len(), 3, "round 3 fixes on its own");
    let b = r.blocked();
    assert!(
        b.is_none_or(|(_, why)| !why.contains("exhausted")),
        "the cap of 3 holds"
    );
}

fn daemon(ws: &TestWorkspace, herdr: Arc<FakeHerdr>) -> qq_core::daemon::DaemonHandle {
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone(),
    });
    let mut deps = Deps::new(herdr, ws.runs_dir(), Registry::native());
    deps.watch = Arc::new(|_, _| Ok(()));
    pipeline_io::register(&mut d, deps).expect("register");
    d.spawn().expect("daemon starts")
}

fn call(ws: &TestWorkspace, args: Value) -> rpc::Response {
    rpc::call(
        &rpc::socket_path(&ws.runs_dir()),
        &Request::new("pipeline", args),
    )
    .expect("rpc transport")
}

#[test]
fn resume_verb_signals_a_live_flow_and_refuses_the_rest() {
    let ws = TestWorkspace::new("pflags-verb");
    let fx = Fixture::new("verb");
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    seed(&mut conn);
    conn.execute("INSERT INTO project(slug, created_ms) VALUES ('p', 1)", [])
        .unwrap();
    let herdr = Arc::new(FakeHerdr::new());
    let d = daemon(&ws, herdr.clone());
    let start = json!({
        "task": "t1", "builder_kind": "ni", "reviewer_kind": "sude",
        "brief": "Goal: x\n", "repo": fx.repo.to_string_lossy(), "branch": "t1",
        "integ": "integ", "pane": "p1", "project_id": 1, "plan": true,
    });
    assert!(call(&ws, start).ok);
    let t0 = Instant::now();
    while herdr.prompts().is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(10), "no plan prompt");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(herdr.prompts()[0].1.contains("PLAN"));

    let ok = call(&ws, json!({ "resume": "t1", "rounds": 3 }));
    assert!(ok.ok, "{:?}", ok.error);
    assert_eq!(ok.result["resumed"], json!(true));
    let unknown = call(&ws, json!({ "resume": "nope" }));
    assert_eq!(unknown.error.unwrap().code, "not_found");
    let zero = call(&ws, json!({ "resume": "t1", "rounds": 0 }));
    assert!(!zero.ok);

    // the command: a good resume reaches the live flow (exit 0), a bad
    // --rounds or a missing task never does (exit 1)
    let mut reg = CliRegistry::new();
    let mut unused = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: Arc::new(FakeHerdr::new()),
    });
    (pipeline_io::INSTALL)(&mut reg, &mut unused).unwrap();
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let run = |a: &[&str]| {
        let args: Vec<String> = a.iter().map(|s| s.to_string()).collect();
        reg.dispatch(&env, &args)
    };
    assert_eq!(run(&["pipeline", "--resume", "t1"]), 0);
    assert_eq!(run(&["pipeline", "--resume", "t1", "--rounds", "2"]), 0);
    assert_eq!(
        run(&["pipeline", "--resume", "t1", "/repo", "--rounds=2"]),
        0
    );
    for bad in [
        &["pipeline", "--resume", "t1", "--rounds", "0"][..],
        &["pipeline", "--resume", "t1", "--rounds", "x"],
        &["pipeline", "--resume", "t1", "--rounds"],
        &["pipeline", "--resume"],
        &["pipeline", "--rounds=2"],
        &["pipeline", "--resume", "nope"],
    ] {
        assert_eq!(run(bad), 1, "{bad:?}");
    }
    d.stop().unwrap();
}
