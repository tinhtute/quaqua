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

fn noop_journal(_: &str, _: serde_json::Value) {}

fn idle_clock<'a>(h: &'a FakeHerdr, status: &'static str) -> FakeClock<'a> {
    FakeClock {
        now: 0.into(),
        on_sleep: Box::new(move |_| h.set_status("w", status)),
    }
}

#[test]
fn watchdog_reports_a_fatal_provider_error_at_once_on_an_idle_pane() {
    let (runs, _c) = seeded("atonce-idle");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    h.set_pane_text("p1", "Not logged in · Please run /login\n");
    let clock = idle_clock(&h, "idle");
    assert_eq!(prompt::wait(&h, &clock, &cfg(&runs, &noop_journal)), Exit::ProviderError);
    assert!(clock.now_sec() < 30, "reported after {}s, not via the idle/stall clock", clock.now_sec());
}

#[test]
fn watchdog_reports_a_fatal_provider_error_at_once_on_a_working_pane() {
    let (runs, _c) = seeded("atonce-working");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "working");
    std::fs::write(prompt::error_path(&runs, "w"), "fatal 401 0\n").unwrap();
    let clock = idle_clock(&h, "working");
    assert_eq!(prompt::wait(&h, &clock, &cfg(&runs, &noop_journal)), Exit::ProviderError);
    assert!(clock.now_sec() < 30, "reported after {}s", clock.now_sec());
}

#[test]
fn watchdog_reports_a_dead_agent_at_once() {
    let (runs, c) = seeded("atonce-dead");
    c.execute(
        "INSERT INTO agent (name, role, kind, started_ms) VALUES ('w', 'peer', 'tieude', 1)",
        [],
    )
    .unwrap();
    let h = FakeHerdr::new(); // herdr has no such agent
    let clock = FakeClock { now: 0.into(), on_sleep: Box::new(|_| {}) };
    assert_eq!(prompt::wait(&h, &clock, &cfg(&runs, &noop_journal)), Exit::CoreDied);
    assert!(clock.now_sec() < 30, "reported after {}s", clock.now_sec());
}

fn set_stall(c: &Connection, v: &str) {
    c.execute(
        "INSERT OR REPLACE INTO setting (key, value_json) VALUES ('notify.stall_sec', ?1)",
        [v],
    )
    .unwrap();
}

fn silent_wait(runs: &Path) -> (Exit, u64) {
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "waiting");
    h.set_pane_text("p1", "nothing new\n");
    let clock = FakeClock { now: 0.into(), on_sleep: Box::new(|_| {}) };
    let exit = prompt::wait(&h, &clock, &cfg(runs, &noop_journal));
    (exit, clock.now_sec())
}

#[test]
fn stall_limit_comes_from_the_setting() {
    let (runs, c) = seeded("stall-set");
    set_stall(&c, "30");
    let (exit, at) = silent_wait(&runs);
    assert_eq!(exit, Exit::Stalled);
    assert!((30..40).contains(&at), "stalled at {at}s, want about 30");
}

#[test]
fn stall_limit_defaults_to_600() {
    let (runs, _c) = seeded("stall-default");
    let (exit, at) = silent_wait(&runs);
    assert_eq!(exit, Exit::Stalled);
    assert!((600..610).contains(&at), "stalled at {at}s, want about 600");
}

const REAL: &str = "Outcome: COMPLETE\n## Changes\nx\n## Verify\nx\n## Risks\nnone\n## Lessons\nnone\n";

#[test]
fn watchdog_resume_ignores_a_fatal_line_already_on_the_pane_while_working() {
    let (runs, _c) = seeded("stale-fatal");
    let (_h, r) = dead_worker(&runs);
    assert_eq!(r.unwrap_err().code, 7);
    // the human logged in; the worker works on, but the old line is still on the pane
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "working");
    h.set_pane_text("p1", "Not logged in · Please run /login\n");
    let report = runs.join("w.md");
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|now| {
            if now >= 30 {
                std::fs::write(&report, REAL).unwrap();
            }
        }),
    };
    let out = prompt::prompt_run(&h, &clock, &runs, &resume(), &tuning()).unwrap();
    assert_eq!(out, report.display().to_string());
    assert!(now_is(&report, REAL));
}

#[test]
fn watchdog_a_working_agent_is_not_cut_off_by_pane_text() {
    let (runs, _c) = seeded("working-pane");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "working");
    h.set_pane_text("p1", "Not logged in · Please run /login\n");
    let report = runs.join("w.md");
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|now| {
            if now >= 30 {
                std::fs::write(&report, REAL).unwrap();
            }
        }),
    };
    assert_eq!(prompt::wait(&h, &clock, &cfg(&runs, &noop_journal)), Exit::ReportReady);
}

#[test]
fn watchdog_exhausted_record_survives_a_working_poll() {
    let (runs, _c) = seeded("record-kept");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "working");
    let rec = prompt::error_path(&runs, "w");
    std::fs::write(&rec, "retryable 529 3\n").unwrap();
    let kept_while_working = std::cell::Cell::new(false);
    let events = std::cell::RefCell::new(Vec::<(String, serde_json::Value)>::new());
    let journal = |k: &str, p: serde_json::Value| events.borrow_mut().push((k.to_string(), p));
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|now| {
            if now < 20 {
                kept_while_working.set(rec.exists());
                h.set_status("w", "working");
            } else {
                h.set_status("w", "idle");
            }
        }),
    };
    assert_eq!(prompt::wait(&h, &clock, &cfg(&runs, &journal)), Exit::Stalled);
    assert!(kept_while_working.get(), "the record was consumed by a working poll");
    let ev = events.borrow();
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0].0, "provider_retry");
    assert_eq!(ev[0].1["harness_exhausted"], true);
}

#[test]
fn stall_limit_of_a_fraction_does_not_stall_a_working_worker() {
    let (runs, c) = seeded("stall-frac-working");
    set_stall(&c, "0.4");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "working");
    let report = runs.join("w.md");
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|now| {
            if now >= 30 {
                std::fs::write(&report, REAL).unwrap();
            }
        }),
    };
    assert_eq!(prompt::wait(&h, &clock, &cfg(&runs, &noop_journal)), Exit::ReportReady);
}

#[test]
fn stall_limit_of_a_fraction_rounds_up_to_a_second() {
    let (runs, c) = seeded("stall-frac");
    set_stall(&c, "0.4");
    let (exit, at) = silent_wait(&runs);
    assert_eq!(exit, Exit::Stalled);
    assert!(at >= 1, "stalled at {at}s");
}

#[test]
fn stall_limit_too_large_falls_back_to_600() {
    let (runs, c) = seeded("stall-huge");
    set_stall(&c, "1e308");
    let (exit, at) = silent_wait(&runs);
    assert_eq!(exit, Exit::Stalled);
    assert!((600..610).contains(&at), "stalled at {at}s, want about 600");
}

#[test]
fn newer_than_keeps_only_lines_after_the_baseline() {
    let base: Vec<String> = ["a", "Not logged in", "login ok"].map(String::from).to_vec();
    assert_eq!(prompt::newer_than(&base, "a\nNot logged in\nlogin ok\n"), "");
    assert_eq!(prompt::newer_than(&base, "a\nNot logged in\nlogin ok\nwork\n"), "work");
    // scrolled: the window lost "a"
    assert_eq!(prompt::newer_than(&base, "Not logged in\nlogin ok\nwork\nmore\n"), "work\nmore");
    // nothing in common: all of it is new
    assert_eq!(prompt::newer_than(&base, "x\ny\n"), "x\ny");
}

#[test]
fn watchdog_stale_fatal_line_is_not_news_once_the_worker_went_idle() {
    let (runs, _c) = seeded("stale-idle");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "working");
    h.set_pane_text("p1", "Not logged in · Please run /login\nlogin ok\n");
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|now| h.set_status("w", if now >= 10 { "idle" } else { "working" })),
    };
    assert_eq!(prompt::wait(&h, &clock, &cfg(&runs, &noop_journal)), Exit::Stalled);
}
