//! The watchdog on a provider error: an unnamed API error gets one resume with
//! backoff, and a worker that dies leaves a report file (never a hidden one).

use std::path::Path;

use qq_core::config;
use qq_core::db;
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::task::prompt::{self, Clock, Exit, Shape, Tuning, WaitCfg};
use rusqlite::Connection;

const SLEEP: &str = "API Error: Your computer went to sleep mid-response";

fn seeded(tag: &str) -> (std::path::PathBuf, Connection) {
    let dir = std::env::temp_dir().join(format!("qq-wd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = db::open(&dir.join(db::path::DB_FILE)).unwrap();
    let team = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
    )
    .unwrap();
    config::seed(&mut c, &team).unwrap();
    (dir, c)
}

fn occupy(h: &FakeHerdr, pane: &str, agent: &str) {
    let r = h.agent_start(agent, "pi", pane, 1, &[]).unwrap();
    assert!(r.ok, "{}", r.body);
}

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

fn cfg<'a>(runs: &'a Path, journal: &'a dyn Fn(&str, serde_json::Value)) -> WaitCfg<'a> {
    WaitCfg {
        name: "w",
        runs_dir: runs,
        report: runs.join("w.md"),
        worktree: None,
        budget_sec: 7200,
        poll_ms: 5_000,
        retry_base_ms: 60_000,
        retry_max: 4,
        harness: Some(Shape::Claude),
        journal,
    }
}

fn tuning() -> Tuning {
    Tuning {
        poll_ms: 5_000,
        retry_base_ms: 60_000,
        retry_max: 4,
    }
}

fn resume() -> Vec<String> {
    vec!["w".into(), "--resume".into()]
}

#[test]
fn watchdog_resumes_an_unnamed_api_error_once_then_escalates() {
    let (runs, _c) = seeded("once");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    h.set_pane_text("p1", &format!("{SLEEP}\n"));
    let events = std::cell::RefCell::new(Vec::<String>::new());
    let journal = |k: &str, _: serde_json::Value| events.borrow_mut().push(k.to_string());
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {
            // the harness echoes our "continue", then fails the same way again
            h.set_pane_text("p1", &format!("{SLEEP}\ncontinue\n{SLEEP}\n"));
            h.set_status("w", "idle");
        }),
    };
    assert_eq!(prompt::wait(&h, &clock, &cfg(&runs, &journal)), Exit::ProviderError);
    assert_eq!(
        *events.borrow(),
        vec!["provider_retry".to_string(), "provider_escalate".to_string()]
    );
    assert_eq!(h.prompts().iter().filter(|(_, t)| t == "continue").count(), 1);
}

#[test]
fn watchdog_never_resumes_a_named_fatal_error() {
    let (runs, _c) = seeded("fatal");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    h.set_pane_text("p1", "Not logged in · Please run /login\n");
    let events = std::cell::RefCell::new(Vec::<String>::new());
    let journal = |k: &str, _: serde_json::Value| events.borrow_mut().push(k.to_string());
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| h.set_status("w", "idle")),
    };
    assert_eq!(prompt::wait(&h, &clock, &cfg(&runs, &journal)), Exit::ProviderError);
    assert_eq!(*events.borrow(), vec!["provider_escalate".to_string()]);
    assert!(h.prompts().iter().all(|(_, t)| t != "continue"));
}

fn dead_worker(runs: &Path) -> (FakeHerdr, Result<String, qq_core::cli::CliError>) {
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    h.set_pane_text("p1", "Not logged in · Please run /login\n");
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| h.set_status("w", "idle")),
    };
    let r = prompt::prompt_run(&h, &clock, runs, &resume(), &tuning());
    drop(clock);
    (h, r)
}

#[test]
fn watchdog_writes_a_stub_report_when_the_worker_dies() {
    let (runs, _c) = seeded("stub");
    let (_h, r) = dead_worker(&runs);
    assert_eq!(r.unwrap_err().code, 7);
    let stub = std::fs::read_to_string(runs.join("w.md")).expect("a report file for a dead worker");
    assert!(stub.starts_with("Outcome: BLOCKED"), "{stub}");
    assert!(stub.contains("signed-out") || stub.contains("provider"), "{stub}");
    assert!(prompt::report_well_formed(&stub));
}

#[test]
fn watchdog_stub_does_not_hide_a_resumed_worker() {
    let (runs, _c) = seeded("resume");
    let (_h, r) = dead_worker(&runs);
    assert_eq!(r.unwrap_err().code, 7);
    // the human fixes the login and resumes; the worker then really finishes
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "working");
    let report = runs.join("w.md");
    let real = "Outcome: COMPLETE\n## Changes\nx\n## Verify\nx\n## Risks\nnone\n## Lessons\nnone\n";
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|now| {
            if now >= 30 {
                std::fs::write(&report, real).unwrap();
            }
        }),
    };
    let out = prompt::prompt_run(&h, &clock, &runs, &resume(), &tuning()).unwrap();
    assert_eq!(out, report.display().to_string());
    assert!(now_is(&report, real), "the stub must not stand in for the real report");
}

fn now_is(p: &Path, want: &str) -> bool {
    std::fs::read_to_string(p).unwrap() == want
}

#[test]
fn watchdog_never_overwrites_a_report_the_worker_wrote() {
    let (runs, _c) = seeded("keep");
    std::fs::write(runs.join("w.md"), "prose only, no outcome\n").unwrap();
    let (_h, r) = dead_worker(&runs);
    assert_eq!(r.unwrap_err().code, 6);
    assert_eq!(std::fs::read_to_string(runs.join("w.md")).unwrap(), "prose only, no outcome\n");
}
