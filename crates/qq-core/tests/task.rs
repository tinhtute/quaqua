//! Per-task flow: build -> review on another harness -> gate -> merge, bound to
//! the candidate sha (R22). The flow runs through the real runner against a
//! real git repo; the herdr and report steps are test-local doubles.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use qq_core::engine::activity::{
    GitRequest, GitResult, HerdrRequest, HerdrResult, ReportFileRequest, ReportFileResult,
};
use qq_core::engine::runner::{run_until_blocked, RunnerOptions, RunnerOutcome};
use qq_core::engine::{
    Activity, ActivityError, ActivityRequest, ActivityResult, InMemoryJournal, VirtualClock,
    WorkflowEvent,
};
use qq_core::merge::{self, Entry, MergeError};
use qq_core::task::pipeline::{PipelineFlow, PipelineInput, PipelineOutput, PipelineState};

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
        Fixture { root, repo, runs }
    }

    /// A commit on the builder branch `t1`, made in a throwaway worktree.
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

    fn input(&self) -> PipelineInput {
        serde_json::from_value(serde_json::json!({
            "task": "t1",
            "builder_kind": "builder",
            "reviewer_kind": "reviewer",
            "brief": "Goal: x\n",
            "repo": self.repo.to_string_lossy(),
            "branch": "t1",
            "integ": "integ",
            "runs_dir": self.runs.to_string_lossy(),
            "pane": "p1",
        }))
        .unwrap()
    }

    fn write(&self, path: String, text: String) {
        std::fs::write(path, text).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The double: herdr and the report files are fakes, git is real.
struct Sim {
    input: PipelineInput,
    log: Mutex<Vec<String>>,
}

impl Sim {
    fn new(input: PipelineInput) -> Self {
        Sim {
            input,
            log: Mutex::new(Vec::new()),
        }
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
        let mut log = self.log.lock().unwrap();
        Ok(match req {
            ActivityRequest::Herdr(HerdrRequest::Prompt { name, .. }) => {
                log.push(format!("prompt {name}"));
                ActivityResult::Herdr(HerdrResult::Prompted { name: name.clone() })
            }
            ActivityRequest::Herdr(HerdrRequest::Spawn { name, kind, .. }) => {
                log.push(format!("spawn {name} {kind}"));
                ActivityResult::Herdr(HerdrResult::Spawned { name: name.clone() })
            }
            ActivityRequest::ReportFile(ReportFileRequest::Read { path }) => {
                ActivityResult::ReportFile(match std::fs::read_to_string(path) {
                    Ok(t) => ReportFileResult::Content(t),
                    Err(_) => ReportFileResult::Exists(false),
                })
            }
            ActivityRequest::Git(GitRequest::CurrentSha) => ActivityResult::Git(GitResult::Sha(
                merge::resolve(Path::new(&self.input.repo), "refs/heads/t1").unwrap_or_default(),
            )),
            ActivityRequest::Git(GitRequest::Merge { branch: sha }) => {
                log.push(format!("merge {sha}"));
                let r = merge::run(&merge::Request {
                    repo: PathBuf::from(&self.input.repo),
                    integ: self.input.integ.clone(),
                    entries: vec![Entry {
                        branch: "t1".into(),
                        sha: Some(sha.clone()),
                    }],
                    check: None,
                });
                ActivityResult::Git(match r {
                    Ok(m) => GitResult::Merged {
                        sha: m[0].head.clone(),
                    },
                    Err(e) => GitResult::Sha(format!("{}: {e}", e.code())),
                })
            }
            other => {
                return Err(ActivityError::ExecutionFailed(format!(
                    "unexpected {other:?}"
                )))
            }
        })
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
    sim: Sim,
    journal: InMemoryJournal,
    clock: VirtualClock,
}

impl Run {
    fn start(fx: &Fixture) -> Self {
        let input = fx.input();
        let mut r = Run {
            sim: Sim::new(input.clone()),
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

    fn signal(&mut self, source: &str, name: &str) -> RunnerOutcome<PipelineOutput> {
        run_until_blocked(
            &PipelineFlow,
            WF,
            None,
            &mut self.journal,
            &mut self.sim,
            &mut self.clock,
            &hold(),
            vec![WorkflowEvent::Signal {
                source: source.into(),
                name: name.into(),
                payload: serde_json::Value::Null,
            }],
        )
        .unwrap()
    }
}

fn build_report(sha: &str) -> String {
    format!("Outcome: COMPLETE\nVerified: unit-test-verified {sha}\n\n## Blockers\nnone\n\n## Lessons\nnone\n")
}

#[test]
fn task_flow_build_review_gate_merge() {
    let fx = Fixture::new("flow");
    let input = fx.input();
    let mut r = Run::start(&fx);

    // build
    let first = fx.commit_on_t1("a.txt");
    r.signal("pipeline", "start");
    assert_eq!(r.sim.saw("prompt t1-b"), 1);
    fx.write(input.build_report(), build_report(&first));
    let out = r.signal("report", "appeared");
    assert!(
        matches!(out, RunnerOutcome::Blocked(_)),
        "review runs: {out:?}"
    );
    // review on the reviewer kind, bound to the candidate
    assert_eq!(r.sim.saw("spawn t1-r reviewer"), 1);
    assert_eq!(r.sim.saw("prompt t1-r"), 1);

    // round 1: one blocking finding -> the builder fixes -> review the new tip
    fx.write(
        input.review_report(1),
        "- [major] handle the empty case\nVerdict: 1 findings\n".into(),
    );
    r.signal("report", "appeared");
    assert_eq!(r.sim.saw("prompt t1-b"), 2, "fix round prompts the builder");
    let second = fx.commit_on_t1("b.txt");
    assert_ne!(first, second);
    fx.write(input.fix_report(1), build_report(&second));
    r.signal("report", "appeared");
    assert_eq!(r.sim.saw("prompt t1-r"), 2, "the new tip is re-reviewed");

    // round 2: clean -> gate -> merge of exactly the reviewed sha
    fx.write(input.review_report(2), "Verdict: clean\n".into());
    let out = r.signal("report", "appeared");
    let RunnerOutcome::Completed(done) = out else {
        panic!("expected merged, got {out:?}")
    };
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
    // main is untouched
    assert!(git(&fx.repo, &["log", "--oneline", "main"]).lines().count() == 1);
}

#[test]
fn acceptance_binds_candidate_sha() {
    // 1. A claim that is not the git tip never reaches review.
    let fx = Fixture::new("claim");
    let input = fx.input();
    let tip = fx.commit_on_t1("a.txt");
    let wrong = "0".repeat(40);
    assert_ne!(tip, wrong);
    let mut r = Run::start(&fx);
    r.signal("pipeline", "start");
    fx.write(input.build_report(), build_report(&wrong));
    let out = r.signal("report", "appeared");
    assert!(matches!(out, RunnerOutcome::Blocked(_)), "{out:?}");
    assert_eq!(r.sim.saw("spawn"), 0, "no review for an unbound claim");
    assert_eq!(r.sim.saw("merge"), 0);

    // 2. A short sha is not a candidate either.
    let fx2 = Fixture::new("short");
    let input2 = fx2.input();
    let tip2 = fx2.commit_on_t1("a.txt");
    let mut r2 = Run::start(&fx2);
    r2.signal("pipeline", "start");
    fx2.write(input2.build_report(), build_report(&tip2[..12]));
    let out = r2.signal("report", "appeared");
    assert!(matches!(out, RunnerOutcome::Blocked(_)), "{out:?}");
    assert_eq!(r2.sim.saw("spawn"), 0);

    // 3. The branch moves after a clean review: acceptance is refused and
    //    nothing is merged.
    let fx3 = Fixture::new("moved");
    let input3 = fx3.input();
    let reviewed = fx3.commit_on_t1("a.txt");
    let mut r3 = Run::start(&fx3);
    r3.signal("pipeline", "start");
    fx3.write(input3.build_report(), build_report(&reviewed));
    r3.signal("report", "appeared");
    assert_eq!(r3.sim.saw("spawn t1-r reviewer"), 1);
    let before = git(&fx3.repo, &["rev-parse", "integ"]);
    let moved = fx3.commit_on_t1("sneaky.txt");
    assert_ne!(reviewed, moved);
    fx3.write(input3.review_report(1), "Verdict: clean\n".into());
    let out = r3.signal("report", "appeared");
    assert!(matches!(out, RunnerOutcome::Blocked(_)), "{out:?}");
    assert_eq!(r3.sim.saw("merge"), 0, "the moved tip is not merged");
    assert_eq!(git(&fx3.repo, &["rev-parse", "integ"]), before);

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
