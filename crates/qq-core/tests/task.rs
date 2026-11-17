//! Per-task flow: build -> review on another harness -> gate -> merge, bound to
//! the candidate sha (R22). The flow runs through the real runner against a
//! real git repo and the production effects; herdr is the fake, the report
//! watcher is a recording closure.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qq_core::config;
use qq_core::daemon::{Config, Daemon};
use qq_core::db;
use qq_core::driver::{HarnessDriver, Registry};
use qq_core::engine::activity::{
    GitRequest, GitResult, HerdrRequest, HerdrResult, ReportFileRequest, ReportFileResult,
};
use qq_core::engine::runner::{run_until_blocked, RunnerOptions, RunnerOutcome};
use qq_core::engine::{
    Activity, ActivityError, ActivityRequest, ActivityResult, InMemoryJournal, Journal,
    VirtualClock, WorkflowEvent,
};
use qq_core::herdr::{FakeHerdr, HerdrClient, Verb};
use qq_core::merge::{self, Entry, MergeError};
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
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// The seeded team (the engine knows no `omp` harness yet: sude runs on
/// opencode here). `ni` builds on claude, `sude` reviews on opencode.
fn seed(conn: &mut rusqlite::Connection) {
    let template =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/workspace/team.toml");
    let team = std::fs::read_to_string(template)
        .unwrap()
        .replace("harness = \"omp\"", "harness = \"opencode\"");
    config::seed(conn, &team).expect("seed");
}

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    runs: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("qq-task-{tag}-{}", std::process::id()));
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

    /// A worktree on `branch` (made once), for commits outside the flow.
    fn worktree(&self, name: &str, branch: &str) -> PathBuf {
        let wt = self.root.join(name);
        if !wt.exists() {
            git(
                &self.repo,
                &["worktree", "add", "-q", &wt.to_string_lossy(), branch],
            );
        }
        wt
    }

    fn commit(&self, wt: &Path, file: &str, content: &str) -> String {
        std::fs::write(wt.join(file), content).unwrap();
        git(wt, &["add", "."]);
        git(wt, &["commit", "-q", "-m", file]);
        git(wt, &["rev-parse", "HEAD"])
    }

    /// A commit on the builder branch `t1`.
    fn commit_on_t1(&self, file: &str) -> String {
        let wt = self.worktree("wt", "t1");
        self.commit(&wt, file, &format!("{file}\n"))
    }

    fn commit_on_integ(&self, file: &str, content: &str) -> String {
        let wt = self.worktree("wti", "integ");
        self.commit(&wt, file, content)
    }

    fn input(&self) -> PipelineInput {
        serde_json::from_value(json!({
            "task": "t1",
            "builder_kind": "ni",
            "reviewer_kind": "sude",
            "brief": "Goal: x\n",
            "repo": self.repo.to_string_lossy(),
            "branch": "t1",
            "integ": "integ",
            "runs_dir": self.runs.to_string_lossy(),
        }))
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A stand-in for the opencode driver, which the engine does not ship yet.
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

/// The native drivers plus the opencode stand-in.
fn drivers() -> Registry {
    let mut r = Registry::native();
    r.register(Opencode);
    r
}

/// The production effects with a fake herdr and a recording watcher. The
/// double only records what goes through.
struct Sim {
    eff: Effects,
    herdr: Arc<FakeHerdr>,
    armed: Arc<Mutex<Vec<String>>>,
    watch_ok: Arc<AtomicBool>,
    log: Mutex<Vec<String>>,
    prompts: Mutex<Vec<(String, String)>>,
}

impl Sim {
    fn new(fx: &Fixture, input: PipelineInput) -> Self {
        let herdr = Arc::new(FakeHerdr::new());
        // the builder is already running when the flow starts (spawn.rs owns that)
        let pane = herdr.seed_pane("work", None, &[]);
        herdr
            .agent_start(&input.builder(), "claude", &pane, 1, &[])
            .unwrap();
        let armed = Arc::new(Mutex::new(Vec::new()));
        let watch_ok = Arc::new(AtomicBool::new(true));
        let (a, ok) = (armed.clone(), watch_ok.clone());
        let deps = Deps {
            herdr: herdr.clone(),
            runs_dir: fx.runs.clone(),
            drivers: drivers(),
            watch: Arc::new(move |wf, path| {
                assert_eq!(wf, WF);
                if !ok.load(Ordering::SeqCst) {
                    return Err("daemon down".into());
                }
                a.lock().unwrap().push(path.to_string());
                Ok(())
            }),
        };
        Sim {
            eff: Effects::new(Arc::new(deps), input),
            herdr,
            armed,
            watch_ok,
            log: Mutex::new(Vec::new()),
            prompts: Mutex::new(Vec::new()),
        }
    }
    /// The text of the n-th prompt sent to `name`.
    fn prompt_text(&self, name: &str, n: usize) -> String {
        let all = self.prompts.lock().unwrap();
        all.iter()
            .filter(|(to, _)| to == name)
            .nth(n)
            .map(|(_, t)| t.clone())
            .unwrap_or_default()
    }
    fn saw(&self, what: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with(what))
            .count()
    }
}

impl Activity for Sim {
    fn execute(&self, req: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        match req {
            ActivityRequest::Herdr(HerdrRequest::Prompt { name, task }) => {
                self.log.lock().unwrap().push(format!("prompt {name}"));
                self.prompts
                    .lock()
                    .unwrap()
                    .push((name.clone(), task.clone()));
            }
            ActivityRequest::Herdr(HerdrRequest::Spawn { name, kind, .. }) => {
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("spawn {name} {kind}"));
                // herdr hosts one agent per pane: the previous round's reviewer
                // has exited after writing its report before the next one starts
                if let Some((task, n)) = name.rsplit_once("-r") {
                    if let Ok(n) = n.parse::<u32>() {
                        self.herdr.remove(&format!("{task}-r{}", n - 1));
                    }
                }
            }
            ActivityRequest::Git(GitRequest::Merge { branch: sha }) => {
                self.log.lock().unwrap().push(format!("merge {sha}"));
            }
            _ => {}
        }
        self.eff.execute(req)
    }
}

/// Time moves only when the test says so: the deadline timer never fires here.
fn hold() -> RunnerOptions {
    RunnerOptions {
        advance_virtual_time: false,
        ..RunnerOptions::default()
    }
}

struct Run {
    /// Every report path the flow armed, across restarts, in order.
    armed: Vec<String>,
    sim: Sim,
    journal: InMemoryJournal,
    clock: VirtualClock,
}

impl Run {
    fn start(fx: &Fixture) -> Self {
        Self::start_with(fx, fx.input())
    }

    fn start_with(fx: &Fixture, input: PipelineInput) -> Self {
        let mut r = Run {
            armed: Vec::new(),
            sim: Sim::new(fx, input.clone()),
            journal: InMemoryJournal::new(),
            clock: VirtualClock::new(1_000_000),
        };
        let out = run_until_blocked(
            &PipelineFlow,
            WF,
            Some(PipelineState::new(input)),
            &mut r.journal,
            &mut r.sim,
            &mut r.clock,
            &hold(),
            Vec::new(),
        )
        .unwrap();
        assert!(
            matches!(out, RunnerOutcome::Blocked(_)),
            "armed and waiting: {out:?}"
        );
        r
    }

    /// A daemon restart: the same journal, new effects.
    fn restart(&mut self, fx: &Fixture) {
        let old: Vec<String> = self.sim.armed.lock().unwrap().drain(..).collect();
        self.armed.extend(old);
        self.sim = Sim::new(fx, fx.input());
    }

    /// The latest snapshot of the flow state (written when the flow pauses).
    fn state(&self) -> Value {
        let snap = self.journal.load_latest_snapshot(WF).unwrap().unwrap();
        serde_json::from_str(&snap.state_json).unwrap()
    }

    /// The report the flow waits for now: the last path it armed.
    fn report(&self) -> String {
        let mut all = self.armed.clone();
        all.extend(self.sim.armed.lock().unwrap().iter().cloned());
        all.last().expect("the flow armed a watcher").clone()
    }

    /// The pause the flow sits in: (code, moved_to), from the latest snapshot.
    fn blocked(&self) -> Option<(i64, Option<String>)> {
        let v = self.state();
        let b = v.get("blocked").filter(|b| !b.is_null())?;
        Some((
            b["code"].as_i64().unwrap(),
            b["moved_to"].as_str().map(String::from),
        ))
    }

    fn armed_paths(&self) -> Vec<String> {
        let mut all = self.armed.clone();
        all.extend(self.sim.armed.lock().unwrap().iter().cloned());
        all
    }

    fn reason(&self) -> String {
        self.state()["blocked"]["reason"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    /// The worker writes the awaited report, the watcher reports it.
    fn answer(&mut self, text: &str) -> RunnerOutcome<PipelineOutput> {
        let path = self.report();
        std::fs::write(&path, text).unwrap();
        self.signal_with("report", "appeared", json!({ "path": path }))
    }

    fn approve(&mut self) -> RunnerOutcome<PipelineOutput> {
        self.deliver(
            WorkflowEvent::ApprovalResolved {
                approved: true,
                reason: None,
            },
            &hold(),
        )
    }

    fn signal(&mut self, source: &str, name: &str) -> RunnerOutcome<PipelineOutput> {
        self.signal_with(source, name, Value::Null)
    }

    fn signal_with(
        &mut self,
        source: &str,
        name: &str,
        payload: Value,
    ) -> RunnerOutcome<PipelineOutput> {
        self.deliver(
            WorkflowEvent::Signal {
                source: source.into(),
                name: name.into(),
                payload,
            },
            &hold(),
        )
    }

    fn deliver(
        &mut self,
        ev: WorkflowEvent,
        opts: &RunnerOptions,
    ) -> RunnerOutcome<PipelineOutput> {
        run_until_blocked(
            &PipelineFlow,
            WF,
            None,
            &mut self.journal,
            &mut self.sim,
            &mut self.clock,
            opts,
            vec![ev],
        )
        .unwrap()
    }
}

fn build_report(sha: &str) -> String {
    format!("Outcome: COMPLETE\nVerified: unit-test-verified {sha}\n\n## Blockers\nnone\n\n## Lessons\nnone\n")
}

fn clean() -> &'static str {
    "Verdict: clean\n"
}

fn merged(out: RunnerOutcome<PipelineOutput>) -> PipelineOutput {
    match out {
        RunnerOutcome::Completed(d) => d,
        other => panic!("expected the flow to finish, got {other:?}"),
    }
}

fn blocked_out(out: &RunnerOutcome<PipelineOutput>) {
    assert!(matches!(out, RunnerOutcome::Blocked(_)), "{out:?}");
}

#[test]
fn task_flow_build_review_gate_merge() {
    let fx = Fixture::new("flow");
    let mut r = Run::start(&fx);

    // build: a stale file at the first path is archived, never read as the report
    let stale = fx.runs.join("t1.build.a1.md");
    std::fs::write(
        &stale,
        "Outcome: COMPLETE\nVerified: unit-test-verified stale\n",
    )
    .unwrap();
    let first = fx.commit_on_t1("a.txt");
    r.signal("pipeline", "start");
    assert_eq!(r.sim.saw("prompt t1-b"), 1);
    assert!(!stale.exists(), "the stale report was moved aside");
    assert!(std::fs::read_dir(&fx.runs).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains(".stale-")));
    let build_path = r.report();
    assert!(build_path.ends_with("t1.build.a1.md"));
    assert!(r.sim.prompt_text("t1-b", 0).contains(&build_path));
    let out = r.answer(&build_report(&first));
    blocked_out(&out);

    // a fresh reviewer for round 1, bound to the candidate
    assert_eq!(r.sim.saw("spawn t1-r1 sude"), 1);
    assert_eq!(r.sim.saw("prompt t1-r1"), 1);
    assert!(
        r.sim.prompt_text("t1-r1", 0).contains(&first),
        "the review prompt is bound to the candidate sha"
    );
    // a late `appeared` of the old build report is not the awaited report:
    // read as the review it would pause the flow (no verdict line)
    let out = r.signal_with("report", "appeared", json!({ "path": build_path }));
    assert!(
        matches!(&out, RunnerOutcome::Blocked(m) if !m.starts_with("pause_for_approval")),
        "{out:?}"
    );

    // round 1: one blocking finding -> the builder fixes -> review the new tip
    r.answer("## Findings\n- [major] handle the empty case\nVerdict: 1 findings\n");
    assert_eq!(r.sim.saw("prompt t1-b"), 2, "fix round prompts the builder");
    let second = fx.commit_on_t1("b.txt");
    assert_ne!(first, second);
    let o = r.answer(&build_report(&second));
    assert!(matches!(o, RunnerOutcome::Blocked(_)), "{o:?}");
    assert_eq!(
        r.sim.saw("spawn t1-r2 sude"),
        1,
        "round 2 has a fresh reviewer"
    );
    assert_eq!(r.sim.saw("prompt t1-r2"), 1, "the new tip is re-reviewed");
    assert!(r.sim.prompt_text("t1-r2", 0).contains(&second));
    assert!(!r.sim.prompt_text("t1-r2", 0).contains(&first));

    // round 2: clean -> gate -> merge of exactly the reviewed sha
    let done = merged(r.answer(clean()));
    assert_eq!(done.status, "merged");
    assert_eq!(done.candidate_sha.as_deref(), Some(second.as_str()));
    assert_eq!(r.sim.saw(&format!("merge {second}")), 1);
    assert_eq!(
        git(&fx.repo, &["merge-base", "--is-ancestor", &second, "integ"]),
        "",
        "the reviewed sha is in the integration branch"
    );
    assert_eq!(
        git(&fx.repo, &["rev-parse", "integ"]),
        done.merge_sha.unwrap()
    );
    assert!(git(&fx.repo, &["log", "--oneline", "main"]).lines().count() == 1);

    // every attempt had its own path, and each one got a watcher
    let armed = r.armed_paths();
    let mut uniq = armed.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(armed.len(), 4, "build, review 1, fix, review 2: {armed:?}");
    assert_eq!(uniq.len(), armed.len(), "no path is armed twice");
}

#[test]
fn acceptance_binds_candidate_sha() {
    // 1. A claim that is not the git tip never reaches review.
    let fx = Fixture::new("claim");
    let tip = fx.commit_on_t1("a.txt");
    let wrong = "0".repeat(40);
    assert_ne!(tip, wrong);
    let mut r = Run::start(&fx);
    r.signal("pipeline", "start");
    let out = r.answer(&build_report(&wrong));
    blocked_out(&out);
    assert_eq!(r.blocked(), Some((7, None)), "bad sha");
    assert!(r.reason().contains("is not the tip"), "{}", r.reason());
    assert_eq!(r.sim.saw("spawn"), 0, "no review for an unbound claim");
    assert_eq!(r.sim.saw("merge"), 0);

    // 2. A short sha is not a candidate either.
    let fx2 = Fixture::new("short");
    let tip2 = fx2.commit_on_t1("a.txt");
    let mut r2 = Run::start(&fx2);
    r2.signal("pipeline", "start");
    let out = r2.answer(&build_report(&tip2[..12]));
    blocked_out(&out);
    assert_eq!(r2.blocked(), Some((7, None)), "short sha");
    assert!(r2.reason().contains("40-hex"), "{}", r2.reason());
    assert_eq!(r2.sim.saw("spawn"), 0);

    // 3. The branch moves after a clean review: acceptance is refused and
    //    nothing is merged.
    let fx3 = Fixture::new("moved");
    let reviewed = fx3.commit_on_t1("a.txt");
    let mut r3 = Run::start(&fx3);
    r3.signal("pipeline", "start");
    r3.answer(&build_report(&reviewed));
    assert_eq!(r3.sim.saw("spawn t1-r1 sude"), 1);
    let before = git(&fx3.repo, &["rev-parse", "integ"]);
    let moved = fx3.commit_on_t1("sneaky.txt");
    assert_ne!(reviewed, moved);
    let out = r3.answer(clean());
    blocked_out(&out);
    assert_eq!(r3.blocked(), Some((8, Some(moved.clone()))), "moved tip");
    assert!(
        r3.reason().contains("moved after review"),
        "the pause says why: {}",
        r3.reason()
    );
    assert_eq!(r3.sim.saw("merge"), 0, "the moved tip is not merged");
    assert_eq!(git(&fx3.repo, &["rev-parse", "integ"]), before);
    // a watcher re-registered after a restart fires again for the report that
    // was already taken: the paused flow does not read it twice
    let (prompts, spawns) = (r3.sim.saw("prompt"), r3.sim.saw("spawn"));
    let again = r3.report();
    r3.signal_with("report", "appeared", json!({ "path": again }));
    assert_eq!(r3.blocked(), Some((8, Some(moved.clone()))), "still paused");
    assert_eq!(
        (r3.sim.saw("prompt"), r3.sim.saw("spawn")),
        (prompts, spawns),
        "no second read, no new work"
    );

    // Approval re-reviews the new tip (a fresh reviewer); the second gate
    // re-reads git and the merge takes the newly reviewed sha, not the first.
    let out = r3.approve();
    blocked_out(&out);
    assert_eq!(r3.sim.saw("spawn t1-r2 sude"), 1);
    assert!(r3.sim.prompt_text("t1-r2", 0).contains(&moved));
    let done = merged(r3.answer(clean()));
    assert_eq!(done.candidate_sha.as_deref(), Some(moved.as_str()));
    assert_eq!(r3.sim.saw(&format!("merge {moved}")), 1);
    assert_eq!(r3.sim.saw(&format!("merge {reviewed}")), 0);
    git(&fx3.repo, &["merge-base", "--is-ancestor", &moved, "integ"]);
    // Rewind so the queue-level checks below run against an unmerged tip.
    git(&fx3.repo, &["branch", "-f", "integ", &before]);

    // 4. The queue itself refuses a stale sha and takes only a full one.
    let stale = merge::run(&merge::Request {
        repo: fx3.repo.clone(),
        integ: "integ".into(),
        entries: vec![Entry {
            branch: "t1".into(),
            sha: Some(reviewed.clone()),
        }],
        check: None,
    });
    assert!(
        matches!(stale, Err(MergeError::ShaMoved { .. })),
        "{stale:?}"
    );
    let short = merge::run(&merge::Request {
        repo: fx3.repo.clone(),
        integ: "integ".into(),
        entries: vec![Entry {
            branch: "t1".into(),
            sha: Some(moved[..10].into()),
        }],
        check: None,
    });
    assert!(
        matches!(short, Err(MergeError::Operational(_))),
        "{short:?}"
    );
    assert_eq!(git(&fx3.repo, &["rev-parse", "integ"]), before);
}

#[test]
fn replay_after_the_branch_moved_reads_the_journal() {
    let fx = Fixture::new("replay");
    let first = fx.commit_on_t1("a.txt");
    let mut r = Run::start(&fx);
    r.signal("pipeline", "start");
    r.answer(&build_report(&first));
    // the branch moves and the daemon restarts: replay takes the journaled
    // candidate, not what git says now
    let moved = fx.commit_on_t1("late.txt");
    r.restart(&fx);
    let out = r.answer(clean());
    blocked_out(&out);
    assert_eq!(r.blocked(), Some((8, Some(moved))));
    assert_eq!(r.state()["reviewed"], json!(first));
    assert_eq!(r.sim.saw("merge"), 0);
    assert_eq!(r.sim.saw("spawn"), 0, "replayed steps are not run again");
}

#[test]
fn rounds_exhausted_approval_grants_one_more_round() {
    let fx = Fixture::new("rounds");
    let mut input = fx.input();
    input.max_rounds = 1;
    let first = fx.commit_on_t1("a.txt");
    let mut r = Run::start_with(&fx, input);
    r.signal("pipeline", "start");
    r.answer(&build_report(&first));
    let out = r.answer("## Findings\n- [major] x\nVerdict: 1 findings\n");
    blocked_out(&out);
    assert_eq!(r.blocked(), Some((5, None)));
    assert!(r.reason().contains("exhausted"), "{}", r.reason());
    assert_eq!(r.sim.saw("prompt t1-b"), 1, "no fix round is free");

    // the Lead grants one more: the builder fixes, a fresh reviewer reviews
    r.approve();
    assert_eq!(r.sim.saw("prompt t1-b"), 2);
    let second = fx.commit_on_t1("b.txt");
    r.answer(&build_report(&second));
    assert_eq!(r.sim.saw("spawn t1-r2 sude"), 1);
    let done = merged(r.answer(clean()));
    assert_eq!(done.status, "merged");
    assert_eq!(done.rounds, 2);
}

#[test]
fn merge_conflict_goes_back_to_the_builder_then_is_re_reviewed() {
    let fx = Fixture::new("conflict");
    fx.commit_on_integ("a.txt", "integ\n");
    let wt = fx.worktree("wt", "t1");
    let first = fx.commit(&wt, "a.txt", "t1\n");
    let mut r = Run::start(&fx);
    r.signal("pipeline", "start");
    r.answer(&build_report(&first));
    let out = r.answer(clean());
    blocked_out(&out);
    // the refusal is a builder task, not a pause
    assert!(r.blocked().is_none(), "{:?}", r.blocked());
    assert_eq!(r.sim.saw("merge"), 1);
    assert_eq!(r.sim.saw("prompt t1-b"), 2);
    assert!(r.sim.prompt_text("t1-b", 1).contains("merge_conflict"));

    // the builder brings integ in; the new tip is reviewed again
    git(&wt, &["merge", "-s", "ours", "--no-edit", "integ"]);
    let second = git(&wt, &["rev-parse", "HEAD"]);
    r.answer(&build_report(&second));
    assert_eq!(r.sim.saw("spawn t1-r2 sude"), 1);
    assert!(r.sim.prompt_text("t1-r2", 0).contains(&second));
    let done = merged(r.answer(clean()));
    assert_eq!(done.candidate_sha.as_deref(), Some(second.as_str()));
    assert_eq!(r.sim.saw("merge"), 2);
}

#[test]
fn failed_check_goes_back_to_the_builder_then_is_re_reviewed() {
    let fx = Fixture::new("checkfail");
    let mut input = fx.input();
    // passes on the base, fails once the branch brings bad.flag
    input.check = Some("! test -f bad.flag".into());
    let wt = fx.worktree("wt", "t1");
    let first = fx.commit(&wt, "bad.flag", "x\n");
    let mut r = Run::start_with(&fx, input);
    r.signal("pipeline", "start");
    r.answer(&build_report(&first));
    r.answer(clean());
    assert!(r.blocked().is_none());
    assert!(r.sim.prompt_text("t1-b", 1).contains("check_failed"));
    assert_eq!(
        git(&fx.repo, &["rev-parse", "integ"]),
        git(&fx.repo, &["rev-parse", "main"]),
        "the failed merge was undone"
    );

    git(&wt, &["rm", "-q", "bad.flag"]);
    git(&wt, &["commit", "-q", "-m", "drop bad.flag"]);
    let second = git(&wt, &["rev-parse", "HEAD"]);
    r.answer(&build_report(&second));
    let done = merged(r.answer(clean()));
    assert_eq!(done.candidate_sha.as_deref(), Some(second.as_str()));
}

#[test]
fn a_broken_base_holds_and_approval_does_not_retry_the_merge() {
    let fx = Fixture::new("base");
    let mut input = fx.input();
    input.check = Some("false".into());
    let first = fx.commit_on_t1("a.txt");
    let mut r = Run::start_with(&fx, input);
    r.signal("pipeline", "start");
    r.answer(&build_report(&first));
    let out = r.answer(clean());
    blocked_out(&out);
    assert_eq!(r.blocked(), Some((4, None)));
    assert!(r.reason().contains("base_broken"), "{}", r.reason());
    assert_eq!(
        r.sim.saw("prompt t1-b"),
        1,
        "the builder cannot fix the base"
    );
    let done = merged(r.approve());
    assert_eq!(done.status, "held");
    assert_eq!(
        r.sim.saw("merge"),
        1,
        "approval did not run the merge again"
    );
}

#[test]
fn deadline_is_rearmed_after_approval() {
    let fx = Fixture::new("deadline");
    fx.commit_on_t1("a.txt");
    let mut r = Run::start(&fx);
    r.signal("pipeline", "start");
    let out = r.answer(&build_report(&"0".repeat(40)));
    blocked_out(&out);
    assert_eq!(r.blocked().map(|b| b.0), Some(7));
    // time passes over the pause: the first deadline fires into it and is dropped
    let out = run_until_blocked(
        &PipelineFlow,
        WF,
        None,
        &mut r.journal,
        &mut r.sim,
        &mut r.clock,
        &RunnerOptions::default(),
        Vec::new(),
    )
    .unwrap();
    blocked_out(&out);
    // approval re-enters the build; with no report ever written the fresh
    // deadline ends the flow instead of leaving it stuck
    let out = r.deliver(
        WorkflowEvent::ApprovalResolved {
            approved: true,
            reason: None,
        },
        &RunnerOptions::default(),
    );
    assert!(
        matches!(out, RunnerOutcome::Failed { code: 10, .. }),
        "{out:?}"
    );
}

#[test]
fn cancel_ends_the_flow_while_waiting_or_paused() {
    let fx = Fixture::new("cancel");
    let first = fx.commit_on_t1("a.txt");
    let mut r = Run::start(&fx);
    r.signal("pipeline", "start");
    r.answer(&build_report(&first));
    let out = r.signal("pipeline", "cancel");
    assert_eq!(merged(out).status, "cancelled");
    assert_eq!(r.sim.saw("merge"), 0);

    let fx2 = Fixture::new("cancel2");
    fx2.commit_on_t1("a.txt");
    let mut r2 = Run::start(&fx2);
    r2.signal("pipeline", "start");
    r2.answer(&build_report(&"0".repeat(40)));
    assert!(r2.blocked().is_some());
    assert_eq!(merged(r2.signal("pipeline", "cancel")).status, "cancelled");
}

#[test]
fn a_claim_equal_to_the_base_tip_is_no_changes() {
    let fx = Fixture::new("nochange");
    let mut r = Run::start(&fx);
    r.signal("pipeline", "start");
    let base = git(&fx.repo, &["rev-parse", "integ"]);
    assert_eq!(base, git(&fx.repo, &["rev-parse", "t1"]));
    let done = merged(r.answer(&build_report(&base)));
    assert_eq!(done.status, "no_changes");
    assert_eq!(r.sim.saw("spawn"), 0, "nothing to review");
}

#[test]
fn production_effects_encode_git_facts_refusals_and_watchers() {
    let fx = Fixture::new("effects");
    let sim = Sim::new(&fx, fx.input());
    let tip = fx.commit_on_t1("a.txt");
    let base = git(&fx.repo, &["rev-parse", "integ"]);

    // the branch tip and the integration tip travel in one result
    let r = sim
        .execute(&ActivityRequest::Git(GitRequest::CurrentSha))
        .unwrap();
    assert_eq!(
        r,
        ActivityResult::Git(GitResult::Sha(format!("{tip} {base}")))
    );

    // a refusal is a coded `Sha`, never an `Err`
    let sha_of = |res: Result<ActivityResult, ActivityError>| match res.unwrap() {
        ActivityResult::Git(GitResult::Sha(s)) => s,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let stale = sha_of(sim.execute(&ActivityRequest::Git(GitRequest::Merge {
        branch: "0".repeat(40),
    })));
    assert!(stale.starts_with("sha_moved: "), "{stale}");
    fx.commit_on_integ("a.txt", "integ\n");
    let tip2 = git(&fx.repo, &["rev-parse", "t1"]);
    let clash = fx.commit(&fx.worktree("wt", "t1"), "a.txt", "t1\n");
    assert_ne!(tip2, clash);
    let conflict = sha_of(sim.execute(&ActivityRequest::Git(GitRequest::Merge { branch: clash })));
    assert!(conflict.starts_with("merge_conflict: "), "{conflict}");

    // arming moves a stale file aside, registers the watcher, and reports a
    // watcher that could not be registered
    let path = fx.runs.join("t1.build.a9.md");
    std::fs::write(&path, "old").unwrap();
    let p = path.to_string_lossy().to_string();
    let armed = sim
        .execute(&ActivityRequest::ReportFile(ReportFileRequest::Exists {
            path: p.clone(),
        }))
        .unwrap();
    assert_eq!(
        armed,
        ActivityResult::ReportFile(ReportFileResult::Exists(true))
    );
    assert!(!path.exists());
    assert_eq!(sim.armed.lock().unwrap().clone(), vec![p.clone()]);
    sim.watch_ok.store(false, Ordering::SeqCst);
    let down = sim
        .execute(&ActivityRequest::ReportFile(ReportFileRequest::Exists {
            path: p,
        }))
        .unwrap();
    assert_eq!(
        down,
        ActivityResult::ReportFile(ReportFileResult::ArmFailed("daemon down".into()))
    );
}

#[test]
fn reviewer_respawn_on_restart_reuses_the_open_agent() {
    let fx = Fixture::new("respawn");
    let sim = Sim::new(&fx, fx.input());
    let spawn = || {
        sim.execute(&ActivityRequest::Herdr(HerdrRequest::Spawn {
            name: "t1-r1".into(),
            kind: "sude".into(),
            repo: String::new(),
            approve_kinds: Vec::new(),
        }))
        .unwrap()
    };
    let ok = ActivityResult::Herdr(HerdrResult::Spawned {
        name: "t1-r1".into(),
    });
    assert_eq!(spawn(), ok);
    // herdr would now refuse a second start: the open agent is reused instead
    sim.herdr.set_fault(Verb::Start, std::io::ErrorKind::Other);
    assert_eq!(spawn(), ok);
    let conn = db::open(&fx.runs.join(db::path::DB_FILE)).unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM agent WHERE name = 't1-r1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn integ_branch_of_a_rust_tree_gets_the_cargo_check() {
    let fx = Fixture::new("defcheck");
    assert_eq!(
        merge::resolve_check(None, "p-integ", &fx.repo),
        None,
        "no Cargo.toml"
    );
    std::fs::write(fx.repo.join("Cargo.toml"), "").unwrap();
    assert_eq!(
        merge::resolve_check(None, "p-integ", &fx.repo).as_deref(),
        Some("cargo check --workspace --locked")
    );
    assert_eq!(
        merge::resolve_check(None, "integ", &fx.repo),
        None,
        "not a *-integ"
    );
    assert_eq!(
        merge::resolve_check(Some("make"), "p-integ", &fx.repo).as_deref(),
        Some("make")
    );
    assert_eq!(
        merge::resolve_check(Some(" "), "p-integ", &fx.repo),
        None,
        "explicit none"
    );
}

#[test]
fn merge_is_idempotent_and_undoes_a_failed_check() {
    let fx = Fixture::new("queue");
    let tip = fx.commit_on_t1("a.txt");
    let req = |check: Option<&str>| merge::Request {
        repo: fx.repo.clone(),
        integ: "integ".into(),
        entries: vec![Entry {
            branch: "t1".into(),
            sha: Some(tip.clone()),
        }],
        check: check.map(String::from),
    };
    let before = git(&fx.repo, &["rev-parse", "integ"]);
    // a failing check undoes the merge
    assert!(matches!(
        merge::run(&req(Some("false"))),
        Err(MergeError::BaseBroken { .. })
    ));
    assert_eq!(git(&fx.repo, &["rev-parse", "integ"]), before);
    // a repeat converges: the second run finds it already in
    let a = merge::run(&req(None)).unwrap();
    let b = merge::run(&req(None)).unwrap();
    assert!(a[0].merged_now && !b[0].merged_now);
    assert_eq!(a[0].head, b[0].head);
}

/// A daemon over `runs` with the pipeline verb; `watch` records registrations.
fn daemon_with(
    ws: &TestWorkspace,
    herdr: Arc<FakeHerdr>,
    watch: Option<Arc<Mutex<Vec<String>>>>,
) -> qq_core::daemon::DaemonHandle {
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone(),
    });
    let watcher: qq_core::daemon::ReportWatcher = match watch {
        Some(seen) => Arc::new(move |_, path| {
            seen.lock().unwrap().push(path.to_string());
            Ok(())
        }),
        None => d.report_watcher(),
    };
    let deps = Deps::new(herdr, ws.runs_dir(), Registry::native(), watcher);
    pipeline_io::register(&mut d, deps).expect("register pipeline");
    d.spawn().expect("daemon starts")
}

fn verb_call(ws: &TestWorkspace, args: Value) -> rpc::Response {
    rpc::call(
        &rpc::socket_path(&ws.runs_dir()),
        &Request::new("pipeline", args),
    )
    .expect("rpc transport")
}

fn verb_args(fx: &Fixture, builder: &str, reviewer: &str, brief: &str, pane: &str) -> Value {
    json!({
        "task": "t1", "builder_kind": builder, "reviewer_kind": reviewer,
        "brief": brief, "repo": fx.repo.to_string_lossy(), "branch": "t1",
        "integ": "integ", "project_id": 1, "plan": false, "pane": pane,
    })
}

fn refused(r: rpc::Response) -> String {
    assert!(!r.ok, "must be refused");
    r.error.unwrap().code
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed() < Duration::from_secs(10), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The `pipeline` verb through the real daemon: review must run on another
/// harness and on a kind that can review; a repeat converges without a second
/// spawn; another input for the same task conflicts; a cancel ends the flow.
#[test]
fn pipeline_verb_enforces_cross_harness_review_and_converges() {
    let ws = TestWorkspace::new("task-verb");
    let fx = Fixture::new("verb");
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    seed(&mut conn);
    conn.execute("INSERT INTO project(slug, created_ms) VALUES ('p', 1)", [])
        .unwrap();
    let herdr = Arc::new(FakeHerdr::new());
    let anchor = herdr.seed_pane("work", None, &[]);
    let daemon = daemon_with(&ws, herdr.clone(), None);
    let args = |b: &str, r: &str, brief: &str| verb_args(&fx, b, r, brief, &anchor);

    // ni and suhuynh both run on claude; tieude cannot review.
    assert_eq!(
        refused(verb_call(&ws, args("ni", "suhuynh", "Goal: x\n"))),
        "same_harness"
    );
    assert_eq!(
        refused(verb_call(&ws, args("ni", "tieude", "Goal: x\n"))),
        "not_a_reviewer"
    );
    assert!(herdr.prompts().is_empty(), "a refusal starts nothing");

    // ni (claude) builds, sude (opencode) reviews: accepted.
    let first = verb_call(&ws, args("ni", "sude", "Goal: x\n"));
    assert!(first.ok, "{:?}", first.error);
    wait_for("build prompt", || !herdr.prompts().is_empty());
    let again = verb_call(&ws, args("ni", "sude", "Goal: x\n"));
    assert!(again.ok, "a repeat converges: {:?}", again.error);
    assert_eq!(again.result["workflow"], first.result["workflow"]);
    assert_eq!(again.result["running"], json!(true));
    assert_eq!(herdr.prompts().len(), 1, "the repeat prompts nobody again");
    let agents: i64 = conn
        .query_row("SELECT count(*) FROM agent", [], |r| r.get(0))
        .unwrap();
    assert_eq!(agents, 1, "one builder, no second spawn");

    // any other input for the task conflicts, not only the brief
    assert_eq!(
        refused(verb_call(&ws, args("ni", "sude", "Goal: other\n"))),
        "conflict"
    );
    for (key, val) in [
        ("pane", "elsewhere"),
        ("integ", "other"),
        ("check", "true"),
        ("repo", "/elsewhere"),
    ] {
        let mut a = args("ni", "sude", "Goal: x\n");
        a[key] = json!(val);
        let r = verb_call(&ws, a);
        assert!(!r.ok, "{key} must conflict: {:?}", r.result);
        assert_eq!(refused(r), "conflict", "{key}");
    }
    assert_eq!(herdr.prompts().len(), 1);

    // cancel ends the flow; a repeat then reports it finished
    let c = verb_call(&ws, json!({ "cancel": "t1" }));
    assert!(c.ok, "{:?}", c.error);
    wait_for("cancelled flow finishes", || {
        verb_call(
            &ws,
            args(
                "ni", "sude", "Goal: x
",
            ),
        )
        .result["finished"]
            == json!(true)
    });
    assert_eq!(
        refused(verb_call(&ws, json!({ "cancel": "nope" }))),
        "not_found"
    );
    daemon.stop().unwrap();
}

/// A flow revived after a daemon restart registers the watcher of the report
/// it waits for again (the old one died with the daemon).
#[test]
fn revived_flow_rearms_its_report_watcher() {
    let ws = TestWorkspace::new("task-revive");
    let fx = Fixture::new("revive");
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    seed(&mut conn);
    conn.execute("INSERT INTO project(slug, created_ms) VALUES ('p', 1)", [])
        .unwrap();

    let seen1 = Arc::new(Mutex::new(Vec::new()));
    let herdr = Arc::new(FakeHerdr::new());
    let anchor = herdr.seed_pane("work", None, &[]);
    let daemon = daemon_with(&ws, herdr.clone(), Some(seen1.clone()));
    let r = verb_call(&ws, verb_args(&fx, "ni", "sude", "Goal: x\n", &anchor));
    assert!(r.ok, "{:?}", r.error);
    wait_for("first watcher", || !seen1.lock().unwrap().is_empty());
    wait_for("build prompt", || !herdr.prompts().is_empty());
    let path = seen1.lock().unwrap()[0].clone();
    assert!(path.ends_with("t1.build.a1.md"), "{path}");
    daemon.stop().unwrap();

    let seen2 = Arc::new(Mutex::new(Vec::new()));
    let daemon = daemon_with(&ws, Arc::new(FakeHerdr::new()), Some(seen2.clone()));
    wait_for("watcher re-registered after the restart", || {
        seen2.lock().unwrap().contains(&path)
    });
    daemon.stop().unwrap();
}
