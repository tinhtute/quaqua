//! Integration tests for Requirement R6: Thread and Subscription lifecycle.
//!
//! R6 specifies:
//! - `worker spawn` opens thread `quaqua/<name>` and subscribes owner for 14 days.
//! - `worker cleanup` keeps thread + subscription.
//! - `lead spawn` opens thread `quaqua/<slug>-integ` owned by `<slug>-lead` with
//!   both `<slug>-lead` and `cos` subscribed for 14 days.
//! - `lead cleanup` keeps thread + subscriptions.
//! - Each new trigger extends active subscriptions by 14 days; expired subscriptions
//!   are not extended and deliver nothing.

use quaqua::bus::{now_ms, open_bus, FarmBus, NewTrigger};
use quaqua::config::Config;
use quaqua::git::GitOps;
use quaqua::herdr::HerdrOps;
use quaqua::layout::SpawnOrigin;
use quaqua::lead::cleanup::cleanup_lead;
use quaqua::lead::spawn::spawn_lead;
use quaqua::worker::cleanup::cleanup_worker;
use quaqua::worker::spawn::{parse_spawn_args, spawn_worker};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);
static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

const FOURTEEN_DAYS_MS: u64 = 14 * 24 * 60 * 60 * 1000;

fn hermetic_temp_dir(prefix: &str) -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "quaqua-sub-test-{prefix}-{}-{id}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create hermetic temp dir");
    dir
}

fn test_cfg() -> Config {
    let mut cfg = Config::default();
    cfg.kinds.insert(
        "sude".to_string(),
        quaqua::config::Kind {
            harness: "omp".to_string(),
            model: "gemini".to_string(),
            effort: Some("max".to_string()),
            fallback: None,
            can_review: false,
            spawn: "auto".to_string(),
            on_cost_cap: "warn".to_string(),
        },
    );
    cfg
}

struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    vars: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn new(keys: &[&'static str]) -> Self {
        let lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let vars = keys.iter().map(|&k| (k, std::env::var(k).ok())).collect();
        Self { _lock: lock, vars }
    }
    fn set(&self, key: &'static str, val: &str) {
        std::env::set_var(key, val);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.vars {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

struct DummyGit;
impl GitOps for DummyGit {
    fn run_in(&self, _cwd: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
        let stdout = if args.first() == Some(&"rev-parse") {
            "391d347df43909dec802c632156fb6ff23c98815\n"
                .as_bytes()
                .to_vec()
        } else {
            Vec::new()
        };
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout,
            stderr: Vec::new(),
        })
    }
    fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        self.run_in(Path::new("."), args)
    }
    fn rev_parse_commit(&self, _repo: &Path, _base: &str) -> bool {
        true
    }
    fn worktree_list_porcelain(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
        Ok(vec![])
    }
    fn worktree_add(
        &self,
        _repo: &Path,
        _branch: &str,
        wt: &Path,
        _base: Option<&str>,
    ) -> std::io::Result<std::process::Output> {
        let _ = std::fs::create_dir_all(wt);
        self.run_in(_repo, &[])
    }
    fn branch_exists(&self, _repo: &Path, _branch: &str) -> bool {
        false
    }
    fn worktree_add_existing(
        &self,
        _repo: &Path,
        _branch: &str,
        _wt: &Path,
    ) -> std::io::Result<std::process::Output> {
        self.run_in(_repo, &[])
    }
    fn worktree_remove(&self, _repo: &Path, wt: &Path) -> std::io::Result<std::process::Output> {
        let _ = std::fs::remove_dir_all(wt);
        self.run_in(_repo, &[])
    }
    fn remote_list(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
        Ok(vec![])
    }
    fn is_core_bare(&self, _repo: &Path) -> bool {
        false
    }
    fn has_core_worktree(&self, _repo: &Path) -> bool {
        false
    }
    fn enable_worktree_config(&self, _repo: &Path) -> std::io::Result<std::process::Output> {
        self.run_in(_repo, &[])
    }
    fn set_worktree_pushurl_blocked(&self, _wt: &Path, _remote: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn is_git_dir(&self, _cwd: &Path) -> bool {
        true
    }
    fn diff_and_status(&self, _cwd: &Path) -> (String, String) {
        ("dh".to_string(), "clean".to_string())
    }
}

struct MockSubHerdr {
    calls: Mutex<Vec<String>>,
    started: std::sync::atomic::AtomicBool,
}

impl MockSubHerdr {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            started: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl HerdrOps for MockSubHerdr {
    fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        self.calls.lock().unwrap().push(args.join(" "));
        let stdout = if args.first() == Some(&"pane") && args.get(1) == Some(&"get") {
            r#"{"result":{"pane":{"pane_id":"w1:p1","workspace_id":"w1","title":"worker","tokens":{"grid_role":"orchestrator","grid_owner":"cos"}}}}"#
        } else if args.first() == Some(&"pane") && args.get(1) == Some(&"split") {
            r#"{"result":{"pane":{"pane_id":"w1:p2","workspace_id":"w1"}}}"#
        } else if args.first() == Some(&"workspace") && args.get(1) == Some(&"create") {
            r#"{"result":{"workspace":{"workspace_id":"ws-sub-1"},"root_pane":{"pane_id":"w1:p1"}}}"#
        } else if args.first() == Some(&"workspace") && args.get(1) == Some(&"close") {
            r#"{"result":{"status":"ok"}}"#
        } else if args.first() == Some(&"agent") && args.get(1) == Some(&"list") {
            r#"{"result":{"agents":[]}}"#
        } else if args.first() == Some(&"agent") && args.get(1) == Some(&"get") {
            if self.started.load(Ordering::SeqCst) {
                r#"{"result":{"agent":{"name":"test-lead","pane_id":"w1:p1","workspace_id":"w1","agent_status":"working"}}}"#
            } else {
                r#"{"error":{"code":"agent_not_found"}}"#
            }
        } else if args.first() == Some(&"agent") && args.get(1) == Some(&"start") {
            self.started.store(true, Ordering::SeqCst);
            r#"{"result":{}}"#
        } else {
            r#"{"result":{"status":"ok"}}"#
        };
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    fn agent_list(&self) -> std::io::Result<String> {
        Ok(r#"{"result":{"agents":[]}}"#.to_string())
    }

    fn agent_get(&self, name: &str) -> std::io::Result<(bool, String)> {
        if self.started.load(Ordering::SeqCst) {
            Ok((
                true,
                format!(
                    r#"{{"result":{{"agent":{{"name":"{name}","agent_status":"working","pane_id":"w1:p1","workspace_id":"w1"}}}}}}"#
                ),
            ))
        } else {
            Ok((false, r#"{"error":{"code":"agent_not_found"}}"#.to_string()))
        }
    }

    fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> {
        Ok(String::new())
    }

    fn agent_start(
        &self,
        _n: &str,
        _k: &str,
        _p: &str,
        _t: u32,
        _a: &[&str],
    ) -> std::io::Result<std::process::Output> {
        self.started.store(true, Ordering::SeqCst);
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: r#"{"result":{}}"#.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    fn pane_split(
        &self,
        _pane: &str,
        _dir: &str,
        _envs: &[(&str, &str)],
    ) -> std::io::Result<std::process::Output> {
        self.calls.lock().unwrap().push("pane_split".to_string());
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: r#"{"result":{"pane":{"pane_id":"w1:p2"}}}"#.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    fn pane_close(&self, _p: &str) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn pane_report_metadata(&self, _p: &str, _t: &[&str]) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool {
        true
    }

    fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> {
        Ok(String::new())
    }

    fn pane_send_keys(&self, _p: &str, _k: &str) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn agent_send_keys(&self, _n: &str, _key: &str) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn agent_prompt_nowait(&self, _n: &str, _p: &str) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn agent_prompt_wait_background(
        &self,
        _name: &str,
        _prompt: &str,
        _timeout_ms: u32,
        _log_file: std::fs::File,
    ) -> std::io::Result<std::process::Child> {
        unimplemented!()
    }
}

// ---------------------------------------------------------------------------
// 1. Bus level R6 tests: open_thread_and_subscribe, 14 days duration
// ---------------------------------------------------------------------------

#[test]
fn test_r6_bus_open_thread_and_subscribe_14_days() {
    let tmp = hermetic_temp_dir("bus-thread-sub");
    let db_path = tmp.join("quaqua.db");
    let bus = open_bus(&db_path).expect("open fresh bus");

    let now = now_ms();
    let until = now + FOURTEEN_DAYS_MS;
    let thread_id = "quaqua/sude-test-worker";

    bus.open_thread_and_subscribe(thread_id, "cos", "cos", until)
        .expect("open thread and subscribe");

    // Invariant 1: thread row exists with owner cos, state open, last_trigger_ms == now
    let (owner, state): (String, String) = bus
        .conn_for_test()
        .query_row(
            "SELECT owner, state FROM threads WHERE id = ?1",
            [thread_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("query thread");
    assert_eq!(owner, "cos");
    assert_eq!(state, "open");

    // Invariant 2: subscriptions row exists with consumer cos, until_ms == until
    let subs = bus.get_subscriptions(thread_id).expect("get subs");
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].0, "cos");
    assert_eq!(subs[0].1, until);
}

// ---------------------------------------------------------------------------
// 2. Bus level R6 tests: Lead integ thread with dual subscriptions & expiry
// ---------------------------------------------------------------------------

#[test]
fn test_r6_bus_lead_dual_subscriptions_and_expiry() {
    let tmp = hermetic_temp_dir("bus-lead-dual-sub");
    let db_path = tmp.join("quaqua.db");
    let bus = open_bus(&db_path).expect("open fresh bus");

    let now = now_ms();
    let until = now + FOURTEEN_DAYS_MS;
    let integ_thread = "quaqua/myproj-integ";
    let lead_name = "myproj-lead";

    // Open integ thread with Lead subscribed
    bus.open_thread_and_subscribe(integ_thread, lead_name, lead_name, until)
        .expect("open thread for lead");
    // Also subscribe cos
    bus.subscribe(integ_thread, "cos", until)
        .expect("subscribe cos");

    let subs = bus.get_subscriptions(integ_thread).expect("get subs");
    assert_eq!(subs.len(), 2);
    assert_eq!(subs[0], ("cos".to_string(), until));
    assert_eq!(subs[1], (lead_name.to_string(), until));

    // Active subscription check at time `now`
    let active_count: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT count(*) FROM subscriptions WHERE thread_id = ?1 AND until_ms >= ?2",
            rusqlite::params![integ_thread, now as i64],
            |r| r.get(0),
        )
        .expect("count active subs at now");
    assert_eq!(active_count, 2, "both subscriptions active at now");

    // Expiry check: simulate time 15 days later -> both must be expired
    let future_time = (now + FOURTEEN_DAYS_MS + 24 * 3600 * 1000) as i64;
    let expired_active_count: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT count(*) FROM subscriptions WHERE thread_id = ?1 AND until_ms >= ?2",
            rusqlite::params![integ_thread, future_time],
            |r| r.get(0),
        )
        .expect("count active subs in future");
    assert_eq!(
        expired_active_count, 0,
        "both subscriptions expired after 14 days"
    );
}

// ---------------------------------------------------------------------------
// 3. Trigger extends active subscriptions by 14 days
// ---------------------------------------------------------------------------

#[test]
fn test_r6_trigger_extends_active_subscriptions() {
    let tmp = hermetic_temp_dir("trigger-extension");
    let db_path = tmp.join("quaqua.db");
    let bus = open_bus(&db_path).expect("open fresh bus");

    let t0 = 1_000_000_000_000u64;
    let thread_id = "quaqua/worker-ext";

    // Initial subscription until t0 + 14 days
    bus.open_thread_and_subscribe(thread_id, "cos", "cos", t0 + FOURTEEN_DAYS_MS)
        .expect("open sub");

    // Add another subscription that is already expired (until t0 - 1 day)
    let conn = bus.conn_for_test();
    conn.execute(
        "INSERT INTO subscriptions (thread_id, consumer, until_ms) VALUES (?1, 'expired-user', ?2)",
        rusqlite::params![thread_id, (t0 - 86_400_000) as i64],
    )
    .expect("insert expired");
    drop(conn);

    // Record a new trigger at t1 = t0 + 5 days
    let t1 = t0 + 5 * 24 * 3600 * 1000;
    let trigger = NewTrigger::new("worker", "progress", "worker", thread_id, "working on task")
        .with_ts_ms(t1);
    bus.record_trigger(&trigger).expect("record trigger");

    // Invariant: active subscription for 'cos' was extended to t1 + 14 days
    let subs = bus.get_subscriptions(thread_id).expect("get subs");
    let cos_sub = subs.iter().find(|s| s.0 == "cos").unwrap();
    assert_eq!(
        cos_sub.1,
        t1 + FOURTEEN_DAYS_MS,
        "active subscription must be extended to trigger_ts + 14 days"
    );

    // Invariant: already-expired subscription was NOT extended
    let exp_sub = subs.iter().find(|s| s.0 == "expired-user").unwrap();
    assert_eq!(
        exp_sub.1,
        t0 - 86_400_000,
        "expired subscription must not be extended"
    );
}

// ---------------------------------------------------------------------------
// 3b. Extension is monotonic: a backdated trigger never shortens the lease
// ---------------------------------------------------------------------------

#[test]
fn test_r6_trigger_extension_is_monotonic() {
    let tmp = hermetic_temp_dir("extension-monotonic");
    let db_path = tmp.join("quaqua.db");
    let bus = open_bus(&db_path).expect("open fresh bus");

    let t0 = 1_000_000_000_000u64;
    let thread_id = "quaqua/monotonic";
    bus.open_thread_and_subscribe(thread_id, "cos", "cos", t0 + 86_400_000)
        .expect("open sub");

    let t1 = t0 + 3_600_000;
    let later =
        NewTrigger::new("worker", "progress", "worker", thread_id, "later trigger").with_ts_ms(t1);
    assert!(bus.record_trigger(&later).expect("record later trigger"));
    let after_later = bus.get_subscriptions(thread_id).expect("get subs")[0].1;
    assert_eq!(
        after_later,
        t1 + FOURTEEN_DAYS_MS,
        "a new trigger must set the lease to trigger_ts + 14 days"
    );

    let backdated =
        NewTrigger::new("worker", "progress", "worker", thread_id, "backdated").with_ts_ms(t0);
    assert!(bus.record_trigger(&backdated).expect("record backdated"));
    let after_backdated = bus.get_subscriptions(thread_id).expect("get subs")[0].1;
    assert_eq!(
        after_backdated, after_later,
        "a backdated trigger must not shorten the lease"
    );
}

// ---------------------------------------------------------------------------
// 3c. A deduped trigger is not a new trigger: it must not renew the lease
// ---------------------------------------------------------------------------

#[test]
fn test_r6_duplicate_trigger_does_not_extend_subscription() {
    let tmp = hermetic_temp_dir("duplicate-extension");
    let db_path = tmp.join("quaqua.db");
    let bus = open_bus(&db_path).expect("open fresh bus");

    let t0 = 1_000_000_000_000u64;
    let thread_id = "quaqua/dup-ext";
    bus.open_thread_and_subscribe(thread_id, "cos", "cos", t0 + 86_400_000)
        .expect("open sub");

    let t1 = t0 + 3_600_000;
    let first = NewTrigger::new("github-ci", "ci_failed", "github", thread_id, "ci failed")
        .with_ts_ms(t1)
        .with_dedupe_key("gh-run:42");
    assert!(
        bus.record_trigger(&first).expect("record first"),
        "first trigger is new and must be inserted"
    );
    let after_first = bus.get_subscriptions(thread_id).expect("get subs")[0].1;
    assert_eq!(after_first, t1 + FOURTEEN_DAYS_MS);

    let t2 = t1 + 3_600_000;
    let duplicate = NewTrigger::new("github-ci", "ci_failed", "github", thread_id, "ci failed")
        .with_ts_ms(t2)
        .with_dedupe_key("gh-run:42");
    assert!(
        !bus.record_trigger(&duplicate).expect("record duplicate"),
        "duplicate dedupe_key must not insert a row"
    );
    let after_duplicate = bus.get_subscriptions(thread_id).expect("get subs")[0].1;
    assert_eq!(
        after_duplicate, after_first,
        "a deduped trigger must not renew the lease"
    );
}

// ---------------------------------------------------------------------------
// 3d. Expired subscriptions deliver nothing; active ones still do
// ---------------------------------------------------------------------------

#[test]
fn test_r6_delivery_skips_expired_subscriptions() {
    // `now_ms()` reads the process-global `PROMPT_WORKER_CLOCK_FILE`, and
    // `test_r6_clock_injected_expiry_boundary` sets it. Hold the env lock so the
    // clock cannot move between the `now` read below and the delivery reads: a
    // fake clock that jumps backwards makes the expired subscription look active.
    let _clock = EnvGuard::new(&["PROMPT_WORKER_CLOCK_FILE"]);

    let tmp = hermetic_temp_dir("delivery-expiry");
    let db_path = tmp.join("quaqua.db");
    let bus = open_bus(&db_path).expect("open fresh bus");

    let now = now_ms();
    let thread_id = "quaqua/expiry-delivery";
    bus.open_thread_and_subscribe(thread_id, "cos", "active-user", now + FOURTEEN_DAYS_MS)
        .expect("open active sub");
    bus.subscribe(thread_id, "expired-user", now.saturating_sub(1_000))
        .expect("open expired sub");

    let trigger =
        NewTrigger::new("worker", "progress", "worker", thread_id, "still running").with_ts_ms(now);
    assert!(bus.record_trigger(&trigger).expect("record trigger"));

    let active = bus
        .load_unread_triggers("active-user", 0)
        .expect("load active");
    assert_eq!(
        active.len(),
        1,
        "active subscriber must receive the trigger"
    );
    assert_eq!(active[0].thread_id, thread_id);

    let expired = bus
        .load_unread_triggers("expired-user", 0)
        .expect("load expired");
    assert!(
        expired.is_empty(),
        "expired subscription must deliver nothing"
    );

    let mut buf = Vec::new();
    let outcome = bus
        .deliver_page(
            "expired-user",
            quaqua::bus::events::DeliverMode::Advance,
            quaqua::bus::events::DeliverFormat::Plain,
            &mut buf,
        )
        .expect("deliver for expired consumer");
    assert!(
        outcome.triggers.is_empty(),
        "the digest must not render triggers for an expired subscription"
    );
}

// ---------------------------------------------------------------------------
// 3e. Clock-injected boundary: until_ms >= now_ms is inclusive
// ---------------------------------------------------------------------------

#[test]
fn test_r6_clock_injected_expiry_boundary() {
    let tmp = hermetic_temp_dir("clock-expiry");
    let db_path = tmp.join("quaqua.db");
    let bus = open_bus(&db_path).expect("open fresh bus");

    // Whole-second value: now_ms() multiplies the clock file by 1000.
    let until = 1_000_000_000_000u64;
    let thread_id = "quaqua/clock-expiry";
    bus.open_thread_and_subscribe(thread_id, "cos", "cos", until)
        .expect("open sub");
    let trigger = NewTrigger::new("worker", "progress", "worker", thread_id, "on the clock")
        .with_ts_ms(until - FOURTEEN_DAYS_MS);
    assert!(bus.record_trigger(&trigger).expect("record trigger"));

    let clock = tmp.join("clock");
    fs::write(&clock, (until / 1000).to_string()).expect("write clock");
    let guard = EnvGuard::new(&["PROMPT_WORKER_CLOCK_FILE"]);
    guard.set("PROMPT_WORKER_CLOCK_FILE", clock.to_str().unwrap());

    let at_deadline = bus
        .load_unread_triggers("cos", 0)
        .expect("load at deadline");
    assert_eq!(
        at_deadline.len(),
        1,
        "subscription is still active at exactly until_ms"
    );

    fs::write(&clock, ((until + 1000) / 1000).to_string()).expect("advance clock");
    let past_deadline = bus
        .load_unread_triggers("cos", 0)
        .expect("load past deadline");
    assert!(
        past_deadline.is_empty(),
        "one second past until_ms the subscription delivers nothing"
    );
}

// ---------------------------------------------------------------------------
// 4. Worker spawn creates thread and subscription, cleanup keeps it
// ---------------------------------------------------------------------------

#[test]
fn test_r6_worker_spawn_and_cleanup_thread_lifecycle() {
    let temp = hermetic_temp_dir("worker-spawn-sub");
    let root = temp.join("root");
    let runs_dir = root.join("runs");
    let repo_dir = root.join("repo");
    fs::create_dir_all(&runs_dir).unwrap();
    fs::create_dir_all(&repo_dir).unwrap();

    let guard = EnvGuard::new(&[
        "QUAQUA_HOME",
        "QUAQUA_RUNS_DIR",
        "QUAQUA_TEST",
        "GRID_SKIP",
        "HERDR_PANE_ID",
        "QUAQUA_OWNER",
    ]);
    guard.set("QUAQUA_HOME", root.to_str().unwrap());
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    guard.set("QUAQUA_TEST", "1");
    guard.set("GRID_SKIP", "1");
    guard.set("HERDR_PANE_ID", "w1:p1");
    guard.set("QUAQUA_OWNER", "cos");

    let cfg = test_cfg();
    let git = DummyGit;
    let herdr = MockSubHerdr::new();
    let worker_name = "sude-r6-test";

    let opts = parse_spawn_args(&[
        worker_name.to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .expect("parse spawn args");

    let t_before = now_ms();
    let res = spawn_worker(opts, &cfg, &git, &herdr, SpawnOrigin::Cli);
    assert!(res.is_ok(), "spawn_worker must succeed: {:?}", res.err());
    let t_after = now_ms();

    // 1. Verify thread row created
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus after spawn");
    let thread_id = format!("quaqua/{worker_name}");

    let (owner, state): (String, String) = bus
        .conn_for_test()
        .query_row(
            "SELECT owner, state FROM threads WHERE id = ?1",
            [&thread_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("query thread for worker");
    assert_eq!(owner, "cos");
    assert_eq!(state, "open");

    // 2. Verify subscription created for 14 days
    let subs = bus
        .get_subscriptions(&thread_id)
        .expect("get subscriptions");
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].0, "cos");
    let expected_min = t_before + FOURTEEN_DAYS_MS;
    let expected_max = t_after + FOURTEEN_DAYS_MS;
    assert!(
        subs[0].1 >= expected_min && subs[0].1 <= expected_max,
        "subscription until_ms ({}) should be ~14 days from spawn [{}..={}]",
        subs[0].1,
        expected_min,
        expected_max
    );

    // 3. Run worker cleanup -> thread and subscription must remain intact!
    let cleanup_res = cleanup_worker(worker_name, &repo_dir, &git, &herdr);
    assert!(
        cleanup_res.is_ok(),
        "cleanup_worker should succeed: {:?}",
        cleanup_res.err()
    );

    let subs_after = bus
        .get_subscriptions(&thread_id)
        .expect("get subscriptions after cleanup");
    assert_eq!(
        subs_after.len(),
        1,
        "cleanup must keep subscription row intact"
    );
    assert_eq!(subs_after[0].0, "cos");
    assert_eq!(subs_after[0].1, subs[0].1);
}

// ---------------------------------------------------------------------------
// 5. Lead spawn creates integ thread + dual subscriptions, cleanup keeps it
// ---------------------------------------------------------------------------

#[test]
fn test_r6_lead_spawn_and_cleanup_thread_lifecycle() {
    let temp = hermetic_temp_dir("lead-spawn-sub");
    let root = temp.join("root");
    let runs_dir = root.join("runs");
    let repo_dir = root.join("projects/beta");
    fs::create_dir_all(&runs_dir).unwrap();
    fs::create_dir_all(&repo_dir).unwrap();

    // Create projects/INDEX.md
    let index_dir = root.join("projects");
    fs::create_dir_all(&index_dir).unwrap();
    fs::write(
        index_dir.join("INDEX.md"),
        "| slug | name | repo | lead | status |\n| --- | --- | --- | --- | --- |\n| beta | Beta | ~/projects/beta | cos | active |\n",
    )
    .unwrap();
    fs::write(index_dir.join("beta.md"), "# beta project card\n").unwrap();

    let guard = EnvGuard::new(&[
        "QUAQUA_HOME",
        "QUAQUA_RUNS_DIR",
        "QUAQUA_TEST",
        "GRID_SKIP",
        "HERDR_PANE_ID",
        "QUAQUA_OWNER",
        "QUAQUA_LAYOUT_SHADOW",
    ]);
    guard.set("QUAQUA_HOME", root.to_str().unwrap());
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    guard.set("QUAQUA_TEST", "1");
    guard.set("GRID_SKIP", "1");
    guard.set("HERDR_PANE_ID", "w1:p1");
    guard.set("QUAQUA_OWNER", "cos");
    guard.set("QUAQUA_LAYOUT_SHADOW", "1");

    let git = DummyGit;
    let herdr = MockSubHerdr::new();
    let slug = "beta";
    let lead_name = "beta-lead";
    let integ_thread = format!("quaqua/{slug}-integ");

    let t_before = now_ms();
    let res = spawn_lead(&[slug.to_string()], &git, &herdr);
    assert!(res.is_ok(), "spawn_lead must succeed: {:?}", res.err());
    let t_after = now_ms();

    // 1. Verify integ thread created with owner = beta-lead
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus after lead spawn");

    let (owner, state): (String, String) = bus
        .conn_for_test()
        .query_row(
            "SELECT owner, state FROM threads WHERE id = ?1",
            [&integ_thread],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("query integ thread");
    assert_eq!(owner, lead_name);
    assert_eq!(state, "open");

    // 2. Verify dual subscriptions (beta-lead and cos, each 14 days)
    let subs = bus
        .get_subscriptions(&integ_thread)
        .expect("get integ subscriptions");
    assert_eq!(
        subs.len(),
        2,
        "must have exactly 2 subscriptions: beta-lead and cos"
    );
    assert_eq!(subs[0].0, "beta-lead");
    assert_eq!(subs[1].0, "cos");

    let expected_min = t_before + FOURTEEN_DAYS_MS;
    let expected_max = t_after + FOURTEEN_DAYS_MS;
    for sub in &subs {
        assert!(
            sub.1 >= expected_min && sub.1 <= expected_max,
            "subscription for {} until_ms ({}) should be ~14 days from spawn [{}..={}]",
            sub.0,
            sub.1,
            expected_min,
            expected_max
        );
    }

    // 3. Run lead cleanup -> integ thread and both subscriptions must remain intact!
    let cleanup_res = cleanup_lead(&[slug.to_string()], &herdr);
    assert!(
        cleanup_res.is_ok(),
        "cleanup_lead should succeed: {:?}",
        cleanup_res.err()
    );

    let subs_after = bus
        .get_subscriptions(&integ_thread)
        .expect("get subs after cleanup");
    assert_eq!(
        subs_after.len(),
        2,
        "cleanup must keep dual subscriptions intact"
    );
    assert_eq!(subs_after[0].0, "beta-lead");
    assert_eq!(subs_after[1].0, "cos");
    assert_eq!(subs_after[0].1, subs[0].1);
    assert_eq!(subs_after[1].1, subs[1].1);
}
