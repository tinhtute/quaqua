//! R8: the watchdog reports `waiting`, not stall exit 3, for an agent with a
//! live subscription on a thread whose Peer is live or that got a trigger
//! inside the quiet window.
//!
//! Two observables: `peer_waiting` (the bus + herdr verdict a wake feeds into
//! `lead_waiting`) and the workflow's exit code, which must not be 3 while the
//! verdict holds and must stay 3 when it does not.

use quaqua::bus::{open_bus, NewTrigger};
use quaqua::engine::activity::{
    ActivityResult, GitResult, HerdrResult, ReportFileResult, SessionResult,
};
use quaqua::engine::child::SingleWorkerInput;
use quaqua::engine::testkit::{run_to_completion, InMemoryJournal, MockActivities, VirtualClock};
use quaqua::herdr::HerdrOps;
use quaqua::state::WorkerMetadata;
use quaqua::worker::prompt::peer_waiting;
use quaqua::worker::prompt::workflow::{SingleWorkerState, SingleWorkerWorkflow, WatchdogConfig};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};

const LEAD: &str = "proj-lead";
const PEER: &str = "w1";
const INTEG: &str = "quaqua/proj-integ";
const FOURTEEN_DAYS_MS: u64 = 14 * 24 * 60 * 60 * 1000;
/// The acceptance case: the Lead has been quiet for 1400 s.
const QUIET_MS: u64 = 1_400_000;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct RunsDir(PathBuf);

impl RunsDir {
    fn new(tag: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("quaqua-watchdog-{tag}-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create runs dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for RunsDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `herdr agent list` only: the check is one list call, never a per-peer probe.
struct FakeHerdr {
    agents: Vec<String>,
}

impl FakeHerdr {
    fn with_agents(agents: &[&str]) -> Self {
        Self {
            agents: agents.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl HerdrOps for FakeHerdr {
    fn run(&self, _args: &[&str]) -> io::Result<Output> {
        unimplemented!("the peer check must not shell out beyond agent list")
    }
    fn agent_list(&self) -> io::Result<String> {
        let agents: Vec<serde_json::Value> = self
            .agents
            .iter()
            .map(|n| serde_json::json!({ "name": n }))
            .collect();
        Ok(serde_json::json!({ "result": { "agents": agents } }).to_string())
    }
    fn agent_get(&self, _name: &str) -> io::Result<(bool, String)> {
        unimplemented!("the peer check must not probe one agent")
    }
    fn agent_read(&self, _name: &str, _source: &str, _lines: u32) -> io::Result<String> {
        unimplemented!()
    }
    fn agent_start(
        &self,
        _name: &str,
        _kind: &str,
        _pane: &str,
        _timeout_ms: u32,
        _extra_args: &[&str],
    ) -> io::Result<Output> {
        unimplemented!()
    }
    fn pane_split(&self, _cwd: &str, _owner: &str, _envs: &[(&str, &str)]) -> io::Result<Output> {
        unimplemented!()
    }
    fn pane_close(&self, _pane: &str) -> io::Result<Output> {
        unimplemented!()
    }
    fn pane_report_metadata(&self, _pane: &str, _tokens: &[&str]) -> io::Result<Output> {
        unimplemented!()
    }
    fn pane_wait_output(&self, _pane: &str, _pattern: &str, _timeout_ms: u32) -> bool {
        false
    }
    fn pane_read(&self, _pane: &str, _source: &str, _lines: u32) -> io::Result<String> {
        unimplemented!()
    }
    fn pane_send_keys(&self, _pane: &str, _key: &str) -> io::Result<Output> {
        unimplemented!()
    }
    fn agent_send_keys(&self, _name: &str, _key: &str) -> io::Result<Output> {
        unimplemented!()
    }
    fn agent_prompt_nowait(&self, _name: &str, _prompt: &str) -> io::Result<Output> {
        unimplemented!()
    }
    fn agent_prompt_wait_background(
        &self,
        _name: &str,
        _prompt: &str,
        _timeout_ms: u32,
        _log_file: std::fs::File,
    ) -> io::Result<std::process::Child> {
        unimplemented!()
    }
}

/// A Lead subscribed to its project thread, with `peer` recording it as owner —
/// the rows `lead spawn` and `worker spawn` write.
fn lead_on_integ_thread(tag: &str, peer: Option<&str>) -> (RunsDir, u64) {
    let runs = RunsDir::new(tag);
    let now = quaqua::bus::now_ms();
    let bus = open_bus(&runs.path().join("quaqua.db")).expect("open bus");
    bus.open_thread_and_subscribe(INTEG, LEAD, LEAD, now + FOURTEEN_DAYS_MS)
        .expect("open integ thread");
    if let Some(peer) = peer {
        WorkerMetadata::save_owner(runs.path(), peer, LEAD).expect("save owner");
    }
    (runs, now)
}

fn trigger_at(runs: &Path, ts_ms: u64) {
    let bus = open_bus(&runs.join("quaqua.db")).expect("open bus");
    bus.record_trigger(
        &NewTrigger::new("github-ci", "ci_failed", "github", INTEG, "ci failed").with_ts_ms(ts_ms),
    )
    .expect("record trigger");
}

#[test]
fn lead_waiting_while_a_subscribed_threads_peer_is_live() {
    let (runs, now) = lead_on_integ_thread("peer-live", Some(PEER));
    let why = peer_waiting(
        runs.path(),
        LEAD,
        now,
        QUIET_MS,
        &FakeHerdr::with_agents(&[PEER]),
    )
    .expect("a live Peer keeps the Lead waiting");
    assert!(why.contains("peer w1 live"), "reason names the Peer: {why}");
    assert!(why.contains(INTEG), "reason names the thread: {why}");
}

#[test]
fn no_waiting_when_the_peer_is_gone_and_nothing_triggered() {
    let (runs, now) = lead_on_integ_thread("peer-gone", Some(PEER));
    assert_eq!(
        peer_waiting(
            runs.path(),
            LEAD,
            now,
            QUIET_MS,
            &FakeHerdr::with_agents(&[])
        ),
        None
    );
}

#[test]
fn a_trigger_inside_the_quiet_window_keeps_the_lead_waiting() {
    let (runs, now) = lead_on_integ_thread("trigger-fresh", Some(PEER));
    trigger_at(runs.path(), now - 60_000);
    let why = peer_waiting(
        runs.path(),
        LEAD,
        now,
        QUIET_MS,
        &FakeHerdr::with_agents(&[]),
    )
    .expect("a fresh trigger keeps the Lead waiting");
    assert!(
        why.contains("trigger 60s ago"),
        "reason names the trigger: {why}"
    );
}

#[test]
fn a_trigger_older_than_the_quiet_window_does_not() {
    let (runs, now) = lead_on_integ_thread("trigger-stale", Some(PEER));
    trigger_at(runs.path(), now - 2_000_000);
    assert_eq!(
        peer_waiting(
            runs.path(),
            LEAD,
            now,
            QUIET_MS,
            &FakeHerdr::with_agents(&[])
        ),
        None
    );
}

#[test]
fn an_expired_subscription_does_not_count() {
    let runs = RunsDir::new("expired");
    let now = quaqua::bus::now_ms();
    let bus = open_bus(&runs.path().join("quaqua.db")).expect("open bus");
    bus.open_thread_and_subscribe(INTEG, LEAD, LEAD, now - 1)
        .expect("open expired subscription");
    WorkerMetadata::save_owner(runs.path(), PEER, LEAD).expect("save owner");
    assert_eq!(
        peer_waiting(
            runs.path(),
            LEAD,
            now,
            QUIET_MS,
            &FakeHerdr::with_agents(&[PEER])
        ),
        None
    );
}

#[test]
fn no_subscription_means_no_waiting() {
    let runs = RunsDir::new("unsubscribed");
    let now = quaqua::bus::now_ms();
    let _ = open_bus(&runs.path().join("quaqua.db")).expect("open bus");
    WorkerMetadata::save_owner(runs.path(), PEER, LEAD).expect("save owner");
    assert_eq!(
        peer_waiting(
            runs.path(),
            LEAD,
            now,
            QUIET_MS,
            &FakeHerdr::with_agents(&[PEER])
        ),
        None
    );
}

#[test]
fn a_thread_named_after_its_peer_uses_that_peer() {
    let runs = RunsDir::new("named-thread");
    let now = quaqua::bus::now_ms();
    let bus = open_bus(&runs.path().join("quaqua.db")).expect("open bus");
    bus.open_thread_and_subscribe(
        &format!("quaqua/{PEER}"),
        LEAD,
        LEAD,
        now + FOURTEEN_DAYS_MS,
    )
    .expect("open peer thread");
    WorkerMetadata::save_owner(runs.path(), PEER, LEAD).expect("save owner");
    let why = peer_waiting(
        runs.path(),
        LEAD,
        now,
        QUIET_MS,
        &FakeHerdr::with_agents(&[PEER]),
    )
    .expect("the named Peer is live");
    assert!(why.contains("peer w1 live"), "reason names the Peer: {why}");
}

// ---------------------------------------------------------------------------
// The exit code the watchdog reports.
// ---------------------------------------------------------------------------

/// One wake of a Lead that is working but moving nothing: constant diff, no
/// session write, no report.
fn register_wake(activities: &mut MockActivities, poll: u64, lead_waiting: bool) {
    activities.register(
        format!("wake_{poll}"),
        ActivityResult::Herdr(HerdrResult::AgentWait {
            name: LEAD.to_string(),
            status: "working".to_string(),
            seq: poll + 1,
            session_kind: "id".to_string(),
            session_value: "sid".to_string(),
            harness: "omp".to_string(),
            cwd: "/repo".to_string(),
            err: String::new(),
            lead_waiting,
            hook_active: true,
        }),
    );
    activities.register(
        format!("obs_{poll}_report"),
        ActivityResult::ReportFile(ReportFileResult::MtimeMs(0)),
    );
    activities.register(
        format!("obs_{poll}_diff"),
        ActivityResult::Git(GitResult::DiffHash {
            hash: "dh0".to_string(),
            stat: String::new(),
        }),
    );
    activities.register(
        format!("obs_{poll}_session"),
        ActivityResult::Session(SessionResult::Mtime(Some(1000))),
    );
}

/// Runs the real workflow for a Lead quiet past the 600 s stall threshold, with
/// `lead_waiting` as the bus check decides it.
fn run_quiet_lead(lead_waiting: bool) -> (i32, u64) {
    let workflow = SingleWorkerWorkflow::new();
    let mut clock = VirtualClock::new(100_000);
    let mut activities = MockActivities::new();
    let mut journal = InMemoryJournal::new();
    for poll in 0..40u64 {
        register_wake(&mut activities, poll, lead_waiting);
    }
    let config = WatchdogConfig {
        poll_sec: 60,
        stall_sec: 600,
        hard_cap_sec: 7200,
        nudge_pct: 100,
        ..WatchdogConfig::default()
    };
    let input = SingleWorkerInput::new(
        LEAD,
        "sude",
        "/repo",
        "Goal: wait for the Peers\nHard constraints: none\nEvidence: none",
        1800,
        None,
    );
    let state = SingleWorkerState::new(input).with_config(config);
    let out = run_to_completion(
        &workflow,
        "test-r8",
        state,
        &mut clock,
        &mut activities,
        &mut journal,
    )
    .expect("workflow completes");
    (out.exit, clock.now_ms())
}

#[test]
fn watchdog_keeps_watching_a_lead_quiet_for_1400s_with_a_live_peer() {
    let (runs, now) = lead_on_integ_thread("exit-live-peer", Some(PEER));
    let herdr = FakeHerdr::with_agents(&[PEER]);
    let waiting = peer_waiting(runs.path(), LEAD, now, QUIET_MS, &herdr).is_some();
    assert!(waiting, "fixture must be the waiting case");

    let (exit, now_ms) = run_quiet_lead(waiting);
    assert_ne!(exit, 3, "a Lead waiting on a live Peer is not stalled");
    assert!(
        now_ms >= 100_000 + QUIET_MS,
        "the run must pass the 1400 s quiet mark, ended at {now_ms}"
    );
    // The budget, not a stall, is what ends the wait.
    assert_eq!(exit, 4);
}

#[test]
fn watchdog_still_exits_3_when_the_peer_is_gone_and_nothing_triggered() {
    let (runs, now) = lead_on_integ_thread("exit-peer-gone", Some(PEER));
    let herdr = FakeHerdr::with_agents(&[]);
    let waiting = peer_waiting(runs.path(), LEAD, now, QUIET_MS, &herdr).is_some();
    assert!(!waiting, "fixture must be the not-waiting case");

    let (exit, now_ms) = run_quiet_lead(waiting);
    assert_eq!(exit, 3, "no live Peer and no trigger keeps the stall");
    assert_eq!(
        now_ms,
        100_000 + 600_000,
        "the stall fires at the 600 s threshold, as today"
    );
}
