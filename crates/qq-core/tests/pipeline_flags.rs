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
use qq_core::identity::peer::HerdrPeers;
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
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml");
    let team = std::fs::read_to_string(template).unwrap();
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
            "runs_dir": self.runs.to_string_lossy(),
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
        Self::start_with(fx, input, |_| {})
    }

    /// [`Run::start`] with `setup` run on the fake herdr after the builder is up.
    fn start_with(fx: &Fixture, input: PipelineInput, setup: impl FnOnce(&FakeHerdr)) -> Self {
        let herdr = Arc::new(FakeHerdr::new());
        let pane = herdr.seed_pane("work", None, &[]);
        herdr
            .agent_start(&input.builder(), "claude", &pane, 1, &[])
            .unwrap();
        setup(&herdr);
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
    assert_ne!(
        code,
        i64::from(qq_core::task::pipeline::BLOCK_UNFINISHED),
        "a consumer keyed on the code tells plan-ready from unfinished"
    );
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
    let deps = Deps::new(
        herdr.clone(),
        ws.runs_dir(),
        Registry::native(),
        Arc::new(|_, _| Ok(())),
    );
    pipeline_io::register(&mut d, deps).expect("register");
    d.require_peer(Arc::new(HerdrPeers::new(herdr)));
    d.spawn().expect("daemon starts")
}

/// Agents the daemon can name: the test process "is" the one whose pane its
/// pid is mapped to ([`Agents::become_agent`]).
struct Agents {
    herdr: Arc<FakeHerdr>,
    panes: Vec<(&'static str, String)>,
}

impl Agents {
    fn seed(conn: &rusqlite::Connection, herdr: &Arc<FakeHerdr>) -> Agents {
        for slug in ["p", "q"] {
            conn.execute(
                "INSERT INTO project(slug, created_ms) VALUES (?1, 1)",
                [slug],
            )
            .unwrap();
        }
        let mut panes = Vec::new();
        for (name, role, project) in [
            ("cos", "cos", None),
            ("p-lead", "lead", Some(1)),
            ("q-lead", "lead", Some(2)),
            ("p-peer", "peer", Some(1)),
        ] {
            conn.execute(
                "INSERT INTO agent (name, role, project_id, started_ms) VALUES (?1, ?2, ?3, 1)",
                rusqlite::params![name, role, project],
            )
            .unwrap();
            let pane = herdr.seed_pane(name, None, &[]);
            herdr.agent_start(name, "claude", &pane, 0, &[]).unwrap();
            panes.push((name, pane));
        }
        Agents {
            herdr: herdr.clone(),
            panes,
        }
    }

    fn become_agent(&self, who: &str) {
        for (name, pane) in &self.panes {
            let pid = if *name == who { std::process::id() } else { 0 };
            self.herdr.set_pane_pids(pane, pid, &[]);
        }
    }
}

fn call(ws: &TestWorkspace, args: Value) -> rpc::Response {
    rpc::call(
        &rpc::socket_path(&ws.runs_dir()),
        &Request::new("pipeline", args),
    )
    .expect("rpc transport")
}

/// Starts task `t<n>` through the verb as the CoS, with `plan` as given (absent: omitted),
/// and returns the builder's first prompt.
fn first_builder_prompt(tag: &str, plan: Option<bool>) -> String {
    let ws = TestWorkspace::new(tag);
    let fx = Fixture::new(tag);
    ws.seal_herdr();
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    seed(&mut conn);
    let herdr = Arc::new(FakeHerdr::new());
    Agents::seed(&conn, &herdr).become_agent("cos");
    let d = daemon(&ws, herdr.clone());
    let mut start = json!({
        "task": "t1", "builder_kind": "ni", "reviewer_kind": "sude",
        "brief": "Goal: x\n", "repo": fx.repo.to_string_lossy(), "branch": "t1",
        "integ": "integ",
    });
    if let Some(p) = plan {
        start["plan"] = json!(p);
    }
    let r = call(&ws, start);
    assert!(r.ok, "{:?}", r.error);
    let t0 = Instant::now();
    while herdr.prompts().is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(10), "no builder prompt");
        std::thread::sleep(Duration::from_millis(20));
    }
    let prompt = herdr.prompts()[0].1.clone();
    d.stop().unwrap();
    prompt
}

#[test]
fn a_new_start_plans_first_unless_plan_is_explicitly_off() {
    let default = first_builder_prompt("pflags-plandef", None);
    assert!(
        default.contains("PLAN") && !default.contains("BUILD"),
        "{default}"
    );
    let on = first_builder_prompt("pflags-planon", Some(true));
    assert!(on.contains("PLAN"), "{on}");
    let off = first_builder_prompt("pflags-planoff", Some(false));
    assert!(off.contains("BUILD") && !off.contains("PLAN"), "{off}");
}

#[test]
fn resume_verb_signals_a_live_flow_and_refuses_the_rest() {
    let ws = TestWorkspace::new("pflags-verb");
    let fx = Fixture::new("verb");
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    seed(&mut conn);
    ws.seal_herdr();
    let herdr = Arc::new(FakeHerdr::new());
    let who = Agents::seed(&conn, &herdr);
    who.become_agent("cos");
    let d = daemon(&ws, herdr.clone());
    let start = json!({
        "task": "t1", "builder_kind": "ni", "reviewer_kind": "sude",
        "brief": "Goal: x\n", "repo": fx.repo.to_string_lossy(), "branch": "t1",
        "integ": "integ", "project_id": 1, "plan": true,
    });
    assert!(call(&ws, start).ok);
    let t0 = Instant::now();
    while herdr.prompts().is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(10), "no plan prompt");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(herdr.prompts()[0].1.contains("PLAN"));

    // Only the flow's own Lead (or the CoS) approves: the builder's peers and
    // another project's Lead are refused and the flow stays paused.
    for intruder in ["p-peer", "q-lead"] {
        who.become_agent(intruder);
        let r = call(&ws, json!({ "resume": "t1" }));
        assert_eq!(r.error.expect("refused").code, "refused", "{intruder}");
    }
    who.become_agent("p-lead");
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
    who.become_agent("cos");
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

#[test]
fn wait_blocks_until_the_run_ends_and_exits_by_how_it_ended() {
    let ws = TestWorkspace::new("pflags-wait");
    let fx = Fixture::new("wait");
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    seed(&mut conn);
    ws.seal_herdr();
    let herdr = Arc::new(FakeHerdr::new());
    Agents::seed(&conn, &herdr).become_agent("cos");
    let d = daemon(&ws, herdr.clone());
    let start = json!({
        "task": "t1", "builder_kind": "ni", "reviewer_kind": "sude",
        "brief": "Goal: x\n", "repo": fx.repo.to_string_lossy(), "branch": "t1",
        "integ": "integ", "project_id": 1, "plan": true,
    });
    assert!(call(&ws, start).ok);
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
    // Cancelled: --wait returns only once the flow has ended, and says 3.
    assert_eq!(run(&["pipeline", "--cancel", "t1", "--wait"]), 3);
    let row = |id: &str| {
        let r = rpc::call(
            &rpc::socket_path(&ws.runs_dir()),
            &Request::new("status", Value::Null),
        )
        .unwrap();
        r.result["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["id"] == id)
            .cloned()
            .unwrap()
    };
    assert!(!row("pipeline:t1")["finished_ms"].is_null());
    // Without --wait nothing changes: the same repeat of a finished flow is
    // answered at once (exit 0), and a repeat start with --wait reports how
    // that finished run ended.
    let again = serde_json::to_string(&json!({
        "task": "t1", "builder_kind": "ni", "reviewer_kind": "sude",
        "brief": "Goal: x\n", "repo": fx.repo.to_string_lossy(), "branch": "t1",
        "integ": "integ", "project_id": 1, "plan": true,
    }))
    .unwrap();
    assert_eq!(run(&["pipeline", &again]), 0);
    assert_eq!(run(&["pipeline", &again, "--wait"]), 3);
    d.stop().unwrap();
}

fn agent_pane(herdr: &FakeHerdr, name: &str) -> String {
    let body = herdr.agent_get(name).unwrap().body;
    serde_json::from_str::<Value>(&body).unwrap()["result"]["agent"]["pane_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{name} has no pane: {body}"))
        .to_string()
}

#[test]
fn the_builder_gets_its_own_pane_and_worktree_not_the_callers() {
    let ws = TestWorkspace::new("pflags-place");
    let fx = Fixture::new("place");
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    seed(&mut conn);
    ws.seal_herdr();
    let herdr = Arc::new(FakeHerdr::new());
    let who = Agents::seed(&conn, &herdr);
    who.become_agent("cos");
    let cos_pane = agent_pane(&herdr, "cos");
    let d = daemon(&ws, herdr.clone());
    let r = call(
        &ws,
        json!({
            "task": "t1", "builder_kind": "ni", "reviewer_kind": "sude",
            "brief": "Goal: x\n", "repo": fx.repo.to_string_lossy(), "branch": "t1",
            "integ": "integ", "plan": false,
        }),
    );
    assert!(r.ok, "{:?}", r.error);
    let builder = agent_pane(&herdr, "t1-b");
    assert_ne!(
        builder, cos_pane,
        "the builder is not started in the caller's pane"
    );
    let wt = fx.root.join(".wt").join("t1-b");
    assert_eq!(
        herdr.split_cwds(),
        vec![(builder, wt.to_string_lossy().into_owned())],
        "its pane starts in its worktree"
    );
    assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "t1");
    d.stop().unwrap();
}

#[test]
fn a_caller_mapped_to_an_unnamed_pane_gets_its_builder_split_off_that_pane() {
    let ws = TestWorkspace::new("pflags-anchor");
    let fx = Fixture::new("anchor");
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    seed(&mut conn);
    ws.seal_herdr();
    let herdr = Arc::new(FakeHerdr::new());
    // The CoS pane: only the grid_role=cos token, no herdr agent name.
    let cos_pane = herdr.seed_pane("cos", None, &[("grid_role", "cos")]);
    herdr.set_pane_pids(&cos_pane, std::process::id(), &[]);
    conn.execute(
        "INSERT INTO agent (name, role, started_ms) VALUES ('cos', 'cos', 1)",
        [],
    )
    .unwrap();
    let d = daemon(&ws, herdr.clone());
    let r = call(
        &ws,
        json!({
            "task": "t1", "builder_kind": "ni", "reviewer_kind": "sude",
            "brief": "Goal: x\n", "repo": fx.repo.to_string_lossy(), "branch": "t1",
            "integ": "integ", "plan": false,
        }),
    );
    assert!(r.ok, "{:?}", r.error);
    let builder = agent_pane(&herdr, "t1-b");
    assert_ne!(builder, cos_pane);
    assert_eq!(
        herdr.splits().first().map(|(anchor, _)| anchor.clone()),
        Some(cos_pane),
        "the builder is split off the pane the daemon mapped the caller to"
    );
    d.stop().unwrap();
}

#[test]
fn place_only_places_the_builder_and_sends_no_prompt_and_starts_no_flow() {
    let ws = TestWorkspace::new("pflags-placeonly");
    let fx = Fixture::new("placeonly");
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    seed(&mut conn);
    ws.seal_herdr();
    let herdr = Arc::new(FakeHerdr::new());
    let who = Agents::seed(&conn, &herdr);
    who.become_agent("cos");
    let d = daemon(&ws, herdr.clone());
    let r = call(
        &ws,
        json!({
            "task": "t1", "builder_kind": "ni", "reviewer_kind": "sude",
            "brief": "Goal: x\n", "repo": fx.repo.to_string_lossy(), "branch": "t1",
            "integ": "integ", "plan": false, "place_only": true,
        }),
    );
    assert!(r.ok, "{:?}", r.error);
    let _ = agent_pane(&herdr, "t1-b");
    assert!(fx.root.join(".wt").join("t1-b").exists(), "worktree placed");
    assert!(herdr.prompts().is_empty(), "no prompt was sent");
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM workflow WHERE id = 'pipeline:t1'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    assert_eq!(n, 0, "no flow started");
    d.stop().unwrap();
}

#[test]
fn each_reviewer_round_gets_its_own_pane_and_worktree_at_the_candidate() {
    let fx = Fixture::new("reviewers");
    let tip = fx.commit_on_t1("a.txt");
    let herdr = Arc::new(FakeHerdr::new());
    let home = herdr.seed_pane("work", None, &[]);
    herdr.agent_start("t1-b", "claude", &home, 1, &[]).unwrap();
    let deps = Deps {
        herdr: herdr.clone(),
        runs_dir: fx.runs.clone(),
        drivers: drivers(),
        watch: Arc::new(|_, _| Ok(())),
    };
    let input = fx.input(false, 2);
    let eff = Effects::new(Arc::new(deps), input);
    for round in 1..=2 {
        eff.execute(&ActivityRequest::Herdr(HerdrRequest::Spawn {
            name: format!("t1-r{round}"),
            kind: "sude".into(),
            repo: fx.repo.to_string_lossy().into_owned(),
            approve_kinds: Vec::new(),
        }))
        .unwrap();
    }
    let (b, r1, r2) = (
        agent_pane(&herdr, "t1-b"),
        agent_pane(&herdr, "t1-r1"),
        agent_pane(&herdr, "t1-r2"),
    );
    assert!(b != r1 && b != r2 && r1 != r2, "{b} {r1} {r2}");
    for r in ["t1-r1", "t1-r2"] {
        let wt = fx.root.join(".wt").join(r);
        assert_eq!(
            git(&wt, &["rev-parse", "HEAD"]),
            tip,
            "{r} is at the candidate"
        );
    }
}

/// The daemon runs outside every pane (the script shell starts it), so its own
/// pid maps to no pane and the peer gate refuses it. A pipeline start from a
/// real pane must still arm its report watcher and prompt the builder.
#[test]
fn a_daemon_in_no_pane_arms_the_report_watcher_for_a_pipeline() {
    use std::io::Write;
    use std::process::Stdio;
    let tag = "dn-nopane";
    let ws = TestWorkspace::new(tag);
    let fx = Fixture::new(tag);
    ws.seal_herdr();
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    seed(&mut conn);
    let herdr = Arc::new(FakeHerdr::new());
    let agents = Agents::seed(&conn, &herdr);
    // The real wiring: no test watch closure. The test process (the daemon)
    // is mapped to no pane; only the client child below is, as the CoS.
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone(),
    });
    let deps = Deps::new(
        herdr.clone(),
        ws.runs_dir(),
        Registry::native(),
        d.report_watcher(),
    );
    pipeline_io::register(&mut d, deps).expect("register");
    d.require_peer(Arc::new(HerdrPeers::new(herdr.clone())));
    let handle = d.spawn().expect("daemon starts");

    let input = json!({
        "task": "t1", "builder_kind": "ni", "reviewer_kind": "sude",
        "brief": "Goal: x\n", "repo": fx.repo.to_string_lossy(), "branch": "t1",
        "integ": "integ",
    })
    .to_string();
    let mut client = Command::new("sh")
        .args(["-c", "read _; exec \"$0\" pipeline \"$1\""])
        .arg(env!("CARGO_BIN_EXE_quaqua"))
        .arg(&input)
        .env("QUAQUA_RUNS_DIR", ws.runs_dir())
        .env("QUAQUA_TEST", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn client");
    for (name, pane) in &agents.panes {
        let pid = if *name == "cos" { client.id() } else { 0 };
        herdr.set_pane_pids(pane, pid, &[]);
    }
    client.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let out = client.wait_with_output().expect("client ends");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let t0 = Instant::now();
    while herdr.prompts().is_empty() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "no builder prompt: the report watcher was never armed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.stop().unwrap_or(());
}

#[test]
fn a_builder_prompt_that_never_lands_pauses_with_its_cause_and_waits_for_no_report() {
    use qq_core::herdr::Lose;
    let fx = Fixture::new("lost");
    let r = Run::start_with(&fx, fx.input(false, 2), |h| {
        h.lose_prompts("t1-b", Lose::Always)
    });
    let (code, reason) = r
        .blocked()
        .expect("a prompt that did not land pauses the flow");
    assert_eq!(code, i64::from(qq_core::task::pipeline::PROMPT_FAILED));
    assert!(
        reason.contains("t1-b") && reason.contains("did not land"),
        "{reason}"
    );
}

#[test]
fn a_builder_prompt_that_needs_one_enter_still_runs_the_flow() {
    use qq_core::herdr::Lose;
    let fx = Fixture::new("enter");
    let r = Run::start_with(&fx, fx.input(false, 2), |h| {
        h.lose_prompts("t1-b", Lose::UntilEnter)
    });
    assert!(
        r.blocked().is_none(),
        "one enter lands it: {:?}",
        r.blocked()
    );
    assert_eq!(r.sim.builder_prompts().len(), 1);
}
