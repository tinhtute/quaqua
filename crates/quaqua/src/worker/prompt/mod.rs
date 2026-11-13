//! Worker prompt & watchdog logic (`quaqua worker prompt <name> <task>`).

pub mod brief;
pub mod cost;
pub mod detach;
pub mod observer;
pub mod outcome;
pub mod playbook_check;
pub mod readonly;
pub mod retry;
pub mod verdict;
pub mod watchdog;
pub mod workflow;

pub use crate::util::now_sec;
pub use brief::{
    brief_header_warnings, lead_routing_warnings, named_playbook_with_reason, warn_brief_headers,
    warn_lead_routing,
};
pub use cost::{brief_cost_budget, read_worker_cost, resolve_cost_budget};
pub use observer::{lead_waiting, report_ready};
pub use outcome::{parse_report_outcome, OutcomeLevel, OutcomeParseError};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
pub use watchdog::{is_lessons_heading, nap, prepend_header, validate_report_content};

use crate::engine::activities::SystemActivities;
use crate::engine::activity::{
    Activity, ActivityError, ActivityRequest, ActivityResult, HerdrRequest,
};
use crate::engine::child::SingleWorkerInput;
use crate::engine::journal::Journal;
use crate::engine::journal_sqlite::SqliteJournal;
use crate::engine::runner::RunnerOutcome;
use crate::git::GitOps;
use crate::herdr::{Herdr, HerdrOps};
use crate::liveness::{liveness, Liveness};
use crate::state::WorkerMetadata;
use crate::worker::cleanup::default_runs_dir;
use crate::worker::prompt::cost::CostTracker;
use crate::worker::prompt::workflow::{SingleWorkerState, SingleWorkerWorkflow, WatchdogConfig};
pub const USAGE: &str = "\
# Usage: quaqua worker prompt <name> <task>        submit task brief, wait for runs file
#        quaqua worker prompt --resume <name> [--extend <sec>]
#                                             keep waiting; --extend adds <sec> to
#                                             this name's allowance (persisted in
#                                             journal; the cap still wins)
#        quaqua worker prompt --check-brief \"<task>\"   validate a brief + print its budget";

/// The exit codes a caller branches on (`--help` prints these on top of
/// [`USAGE`]). Each line names the code `workflow`/`retry` raises, and the
/// meanings follow `rules/_all/worker-timebox.md`.
pub const USAGE_HELP: &str = "\
# Exit codes:
#   0  report printed: `runs/<name>.md` is ready and readable
#   1  usage error, or refused before the wait: bad arguments, --check-brief
#      rejected the brief, empty brief, or no live worker
#   2  hard cap: no report before PROMPT_WORKER_MAX_SEC (7200s)
#   3  stall: no progress for PROMPT_WORKER_STALL_SEC (600s), agent idle/done
#      for PROMPT_WORKER_IDLE_SEC (120s), or agent gone
#   4  time budget exceeded (the brief's `Time budget`)
#   5  blocked on a question/permission, or the cost cap was reached on a
#      kind with on_cost_cap = \"kill\"
#   6  malformed report: missing or unknown `Outcome:` line, `## Lessons`, or
#      the `Verdict:` tail; or a pinned playbook step not accounted for
#   7  provider error needing a human: signed out, quota, billing; or a
#      transient error still failing after PROMPT_WORKER_RETRY_MAX (4) resumes";

pub fn brief_headers_ok(text: &str) -> bool {
    text.contains("Goal:")
        && text.contains("Hard constraints:")
        && (text.contains("Current design (open to challenge):")
            || text.contains("Current design:"))
}

pub fn brief_budget_sec(text: &str) -> Option<u64> {
    for line in text.lines() {
        let lower = line.to_lowercase();
        let mut search_from = 0;
        while let Some(idx) = lower[search_from..].find("time budget:") {
            let start = search_from + idx + 12;
            let rest = &lower[start..];
            search_from = start;

            // [^0-9]{0,8} using byte offsets via char_indices to avoid slicing inside multi-byte characters
            let mut digit_start = None;
            let mut non_digit_chars = 0;
            for (byte_offset, c) in rest.char_indices() {
                if c.is_ascii_digit() {
                    digit_start = Some((byte_offset, non_digit_chars));
                    break;
                }
                non_digit_chars += 1;
                if non_digit_chars > 8 {
                    break;
                }
            }

            let Some((byte_offset, _)) = digit_start else {
                continue;
            };

            let after_prefix = &rest[byte_offset..];

            // [0-9]+
            let digit_bytes = after_prefix
                .bytes()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if digit_bytes == 0 {
                continue;
            }

            let num_str = &after_prefix[..digit_bytes];
            let Ok(n) = num_str.parse::<u64>() else {
                continue;
            };

            // [[:space:]]*
            let after_digits = after_prefix[digit_bytes..].trim_start();

            // (min|mins|minutes|m)([^[:alnum:]]|$)
            let matched_unit = if let Some(rem) = after_digits.strip_prefix("minutes") {
                Some(rem)
            } else if let Some(rem) = after_digits.strip_prefix("mins") {
                Some(rem)
            } else if let Some(rem) = after_digits.strip_prefix("min") {
                Some(rem)
            } else {
                after_digits.strip_prefix('m')
            };

            if let Some(rem) = matched_unit {
                let valid_end = match rem.chars().next() {
                    None => true,
                    Some(c) => !c.is_alphanumeric(),
                };
                if valid_end {
                    return Some(n * 60);
                }
            }
        }
    }
    None
}

pub fn validate_env_budget_val(val: Option<&str>) -> Result<(), (i32, String)> {
    if let Some(v) = val {
        if !v.is_empty() && !v.chars().all(|c| c.is_ascii_digit()) {
            return Err((
                1,
                format!("error: PROMPT_WORKER_BUDGET_SEC must be whole seconds (got '{v}')\n"),
            ));
        }
    }
    Ok(())
}

pub fn validate_env_budget() -> Result<(), (i32, String)> {
    let var = std::env::var("PROMPT_WORKER_BUDGET_SEC").ok();
    validate_env_budget_val(var.as_deref())
}

pub fn resolve_budget_sec(task: &str) -> (u64, &'static str) {
    if let Ok(env_val) = std::env::var("PROMPT_WORKER_BUDGET_SEC") {
        if !env_val.is_empty() {
            if let Ok(n) = env_val.parse::<u64>() {
                return (n, "env");
            }
        }
    }
    if let Some(n) = brief_budget_sec(task) {
        return (n, "brief");
    }
    (2700, "default")
}

pub fn do_check_brief(task: &str) -> Result<(), (i32, String)> {
    let runs_dir = default_runs_dir();
    do_check_brief_with_owner(task, None, &runs_dir)
}

pub fn do_check_brief_with_owner(
    task: &str,
    owner: Option<&str>,
    runs_dir: &std::path::Path,
) -> Result<(), (i32, String)> {
    validate_env_budget()?;
    if !brief_headers_ok(task) {
        return Err((
            1,
            "error: task must be a brief with 'Goal:', 'Hard constraints:', 'Current design (open to challenge):' (rules/_all/brief.md)\n".to_string(),
        ));
    }
    // The coordinator's topology choice is part of the contract: a brief that
    // does not say solo or lead cannot be checked against AGENTS.md '## Roles (SLP)'.
    crate::worker::leadgate::topology(task).map_err(|msg| (1, msg))?;
    brief::warn_brief_headers(task);
    brief::warn_main_checkout_path(task);
    // #178: a stated review rule must be valid; an absent one takes the defaults.
    brief::review_policy(task).map_err(|e| (1, e))?;
    brief::warn_lead_routing(task, owner, runs_dir);
    // A name the present index cannot resolve or render is the one rejection
    // here; a missing or incomplete index only warns.
    brief::prepare_injection_in_workspace(task)?;
    // Only the brief carries its budgets, so this is the one place the budget
    // signal can be raised (rules/_all/brief.md).
    for signal in crate::worker::leadgate::brief_signals(task) {
        eprintln!(
            "warning: topology: {} — this brief may need a project Lead (`quaqua lead spawn <slug>`)",
            signal.describe()
        );
    }
    let (budget, src) = resolve_budget_sec(task);
    println!("budget={budget}s source={src}");
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum PromptAction {
    CheckBrief(String),
    Resume { name: String, extend: Option<u64> },
    Prompt { name: String, task: String },
}

pub fn parse_prompt_args(p_args: &[String]) -> Result<PromptAction, (i32, String)> {
    if p_args.is_empty() {
        return Err((1, format!("{USAGE}\n")));
    }
    if p_args[0] == "--check-brief" {
        if p_args.len() != 2 {
            return Err((1, format!("{USAGE}\n")));
        }
        return Ok(PromptAction::CheckBrief(p_args[1].clone()));
    }

    if p_args[0] == "--resume" {
        if p_args.len() < 2 {
            return Err((1, format!("{USAGE}\n")));
        }
        let name = p_args[1].clone();
        let mut extend: Option<u64> = None;
        if p_args.len() > 2 {
            if p_args[2] == "--extend" {
                if p_args.len() != 4 {
                    return Err((1, format!("{USAGE}\n")));
                }
                let sec: u64 = p_args[3].parse().map_err(|_| {
                    (
                        1,
                        "error: --extend <sec> requires a positive integer\n".to_string(),
                    )
                })?;
                extend = Some(sec);
            } else {
                return Err((1, format!("{USAGE}\n")));
            }
        }
        return Ok(PromptAction::Resume { name, extend });
    }

    // Normal prompt: <name> <task>
    if p_args.len() != 2 {
        return Err((1, format!("{USAGE}\n")));
    }
    let name = p_args[0].clone();
    let task = p_args[1].clone();
    if task.trim().is_empty() {
        return Err((1, "error: empty task brief\n".to_string()));
    }
    if !brief_headers_ok(&task) {
        return Err((
            1,
            "error: task must be a brief with 'Goal:', 'Hard constraints:', 'Current design (open to challenge):' (rules/_all/brief.md)\n".to_string(),
        ));
    }
    Ok(PromptAction::Prompt { name, task })
}

#[derive(Debug, Clone, Default)]
struct PromptClock {
    timers: BinaryHeap<Reverse<(u64, u64, String)>>,
    seq: u64,
}

impl PromptClock {
    fn new() -> Self {
        Self {
            timers: BinaryHeap::new(),
            seq: 0,
        }
    }
}

impl crate::engine::runner::RunnerClock for PromptClock {
    fn now_ms(&self) -> u64 {
        if std::env::var_os("PROMPT_WORKER_CLOCK_FILE").is_some() {
            crate::util::now_sec() * 1000
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        }
    }

    fn schedule_timer(&mut self, id: &str, duration: Duration) {
        let deadline = self.now_ms() + duration.as_millis() as u64;
        self.seq = self.seq.wrapping_add(1);
        self.timers
            .push(Reverse((deadline, self.seq, id.to_string())));
    }

    fn advance_to_next_timer(&mut self) -> Option<(u64, Vec<String>)> {
        if self.timers.is_empty() {
            return None;
        }

        loop {
            let now = self.now_ms();
            if let Some(Reverse((earliest, _, _))) = self.timers.peek() {
                if *earliest > now {
                    let delta_ms = *earliest - now;
                    let delta_sec = (delta_ms / 1000).max(1);
                    nap(delta_sec);
                }
            } else {
                return None;
            }

            let now = self.now_ms();
            let mut fired = Vec::new();
            while let Some(Reverse((deadline, _, _))) = self.timers.peek() {
                if *deadline <= now {
                    if let Some(Reverse((_, _, id))) = self.timers.pop() {
                        fired.push(id);
                    }
                } else {
                    break;
                }
            }
            if !fired.is_empty() {
                return Some((now, fired));
            }
            if self.timers.is_empty() {
                return None;
            }
        }
    }

    fn pending_timers_count(&self) -> usize {
        self.timers.len()
    }
}

#[derive(Clone, Default)]
struct WaitSnapshot {
    status: String,
    diffstat: String,
}

/// Reads the status and error code from an `agent prompt --wait` /
/// `agent wait` payload. Tolerant to both `result.agent` and `result` shapes.
fn parse_submit_output(out: &std::process::Output) -> (bool, String, String) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = if stdout.trim().is_empty() {
        stderr.as_ref()
    } else {
        stdout.as_ref()
    };
    let mut status = String::new();
    let mut code = String::new();
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(c) = json.pointer("/error/code").and_then(|v| v.as_str()) {
            code = c.to_string();
        }
        let agent = json
            .pointer("/result/agent")
            .or_else(|| json.get("result").filter(|r| r.is_object()));
        if let Some(a) = agent {
            status = a
                .get("agent_status")
                .or_else(|| a.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
    }
    (out.status.success(), status, code)
}

/// One live subscription of the watched agent, with the newest trigger on that
/// thread inside the quiet window (`None` when nothing landed).
struct LiveSubscription {
    thread_id: String,
    recent_trigger_ms: Option<u64>,
}

/// R8: the watched agent is legitimately waiting on the bus.
///
/// True when `name` holds a live subscription on a thread whose Peer is live
/// (`herdr agent list`) or that received a trigger inside `quiet_ms`. The
/// reason feeds the `WATCHDOG waiting` line; `None` keeps the stall behaviour.
///
/// A thread's Peer is the agent the thread is named after (`quaqua/<peer>`,
/// when that agent is a known worker). Any other thread — the project thread
/// `quaqua/<slug>-integ` — is watched on behalf of the workers that record
/// `name` as their owner: the same set `lead cleanup` refuses to leave behind.
pub fn peer_waiting<H: HerdrOps>(
    runs_dir: &Path,
    name: &str,
    now_ms: u64,
    quiet_ms: u64,
    herdr: &H,
) -> Option<String> {
    let subs = live_subscriptions(&runs_dir.join("quaqua.db"), name, now_ms, quiet_ms);
    if subs.is_empty() {
        return None;
    }
    let mut live: Option<Vec<String>> = None;
    for sub in &subs {
        if let Some(ts_ms) = sub.recent_trigger_ms {
            return Some(format!(
                "thread {}: trigger {}s ago",
                sub.thread_id,
                now_ms.saturating_sub(ts_ms) / 1000
            ));
        }
        let peers = thread_peers(runs_dir, name, &sub.thread_id);
        if peers.is_empty() {
            continue;
        }
        let agents = live.get_or_insert_with(|| live_agents(herdr));
        if let Some(peer) = peers.iter().find(|p| agents.iter().any(|a| a == *p)) {
            return Some(format!("thread {}: peer {peer} live", sub.thread_id));
        }
    }
    None
}

/// R8: the trigger window to hand `peer_waiting`, or `None` when no exit
/// branch could fire yet.
///
/// `quiet_sec` is the time since the last observed progress, `elapsed_sec` the
/// session age (it covers `--resume`, whose stall baseline predates this
/// process), and `status` the agent's resolved state.
///
/// The window is one stall period. Growing it with the session would cover
/// every trigger since the watch started, so a Lead that hung after
/// dispatching its Peers would never reach the stall exit.
pub fn peer_wait_window(
    quiet_sec: u64,
    elapsed_sec: u64,
    status: &str,
    stall_sec: u64,
) -> Option<u64> {
    let near_a_stall =
        quiet_sec >= stall_sec || elapsed_sec >= stall_sec || status == "idle" || status == "done";
    near_a_stall.then(|| stall_sec.saturating_mul(1000))
}

/// Live subscriptions of `consumer`, each with the newest trigger on its thread
/// at or after `now_ms - quiet_ms`.
fn live_subscriptions(
    db_path: &Path,
    consumer: &str,
    now_ms: u64,
    quiet_ms: u64,
) -> Vec<LiveSubscription> {
    if !db_path.exists() {
        return Vec::new();
    }
    // `bus/` exposes no consumer-scoped subscription query, and this unit may
    // not edit it: read the columns the check needs through the shared opener,
    // which carries the live-DB guard and the busy timeout. `threads.last_trigger_ms`
    // is the thread's open time, not a trigger, so the window reads `triggers`.
    let Ok(conn) = crate::engine::sqlite::open_conn(db_path) else {
        return Vec::new();
    };
    let since_ms = now_ms.saturating_sub(quiet_ms);
    crate::engine::sqlite::retry_while_busy(|| {
        let mut stmt = conn.prepare_cached(
            "SELECT t.id, (SELECT MAX(tr.ts_ms) FROM triggers tr \
                           WHERE tr.thread_id = t.id AND tr.ts_ms >= ?3) \
             FROM subscriptions s JOIN threads t ON t.id = s.thread_id \
             WHERE s.consumer = ?1 AND s.until_ms >= ?2 ORDER BY t.id ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![consumer, now_ms as i64, since_ms as i64],
            |r| {
                Ok(LiveSubscription {
                    thread_id: r.get(0)?,
                    recent_trigger_ms: r.get::<_, Option<i64>>(1)?.map(|ms| ms.max(0) as u64),
                })
            },
        )?;
        rows.collect()
    })
    .unwrap_or_default()
}

fn thread_peers(runs_dir: &Path, name: &str, thread_id: &str) -> Vec<String> {
    if let Some(agent) = thread_id.strip_prefix("quaqua/") {
        if agent != name && runs_dir.join(format!("{agent}.owner")).exists() {
            return vec![agent.to_string()];
        }
    }
    owned_workers(runs_dir, name)
}

/// Workers that record `name` as their owner, sorted for a stable reason line.
fn owned_workers(runs_dir: &Path, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(runs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("owner") {
                continue;
            }
            let peer = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if !peer.is_empty() && WorkerMetadata::load_owner(runs_dir, peer) == name {
                out.push(peer.to_string());
            }
        }
    }
    out.sort();
    out
}

/// Agent names from one `herdr agent list` that are actually working the
/// thread: `working` has a turn in flight and `blocked` is sitting on a
/// question. A `done` or `idle` worker whose pane lingers until cleanup has
/// nothing left to answer, so it must not hold the Lead past its stall.
/// An unreadable or empty list proves no Peer live, which keeps the old stall
/// behaviour (absence is not evidence).
fn live_agents<H: HerdrOps>(herdr: &H) -> Vec<String> {
    herdr
        .agent_list()
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.pointer("/result/agents")
                .and_then(|a| a.as_array())
                .cloned()
        })
        .map(|agents| {
            agents
                .iter()
                .filter(|a| {
                    matches!(
                        a.get("agent_status").and_then(|s| s.as_str()),
                        Some("working") | Some("blocked")
                    )
                })
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

struct PromptActivities {
    inner: SystemActivities,
    runs_dir: PathBuf,
    time_budget_sec: u64,
    stall_sec: u64,
    nudge_pct: u64,
    heartbeat_sec: u64,
    start_sec: u64,
    cost_tracker: Mutex<CostTracker>,
    cost_stopped: Mutex<bool>,
    last_wait: Mutex<Option<WaitSnapshot>>,
    last_progress: Mutex<u64>,
    last_diff: Mutex<String>,
    last_heartbeat: Mutex<u64>,
}

impl Activity for PromptActivities {
    fn execute(&self, req: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        if let ActivityRequest::Git(crate::engine::activity::GitRequest::DiffHash { path }) = req {
            let target = std::path::Path::new(path);
            if !target.exists() || !target.is_dir() || !self.inner.git.is_git_dir(target) {
                return Ok(ActivityResult::Git(
                    crate::engine::activity::GitResult::DiffHash {
                        hash: String::new(),
                        stat: String::new(),
                    },
                ));
            }
        }
        if let ActivityRequest::ReportFile(
            crate::engine::activity::ReportFileRequest::CheckMtime { path },
        ) = req
        {
            let p = std::path::Path::new(path);
            if !p.exists() {
                return Ok(ActivityResult::ReportFile(
                    crate::engine::activity::ReportFileResult::MtimeMs(0),
                ));
            }
            return Ok(ActivityResult::ReportFile(
                crate::engine::activity::ReportFileResult::MtimeMs(if report_ready(p) {
                    1
                } else {
                    0
                }),
            ));
        }
        if let ActivityRequest::ReportFile(crate::engine::activity::ReportFileRequest::Read {
            path,
        }) = req
        {
            if !std::path::Path::new(path).exists() {
                return Ok(ActivityResult::ReportFile(
                    crate::engine::activity::ReportFileResult::Content(String::new()),
                ));
            }
        }

        let mut res = self.inner.execute(req);
        match req {
            ActivityRequest::Herdr(HerdrRequest::AgentWait { name, .. }) => {
                let mut stop_now = false;
                if let Ok(ActivityResult::Herdr(
                    crate::engine::activity::HerdrResult::AgentWait {
                        status,
                        err,
                        session_value,
                        harness,
                        cwd,
                        lead_waiting,
                        ..
                    },
                )) = &mut res
                {
                    let now = now_sec();
                    // The agent's own hook report (`runs/.<name>.state`) is the
                    // primary signal; herdr's screen inference is the fallback
                    // when the file is absent or stale. Never override a
                    // herdr error: a gone/failed agent is not a hook reading.
                    if err.is_empty() || err.as_str() == "timeout" {
                        let screen = status.clone();
                        *status = crate::liveness::resolve_worker_state(
                            &self.runs_dir,
                            name,
                            &screen,
                            now,
                            crate::liveness::DEFAULT_STATE_STALENESS_SEC,
                        )
                        .as_str()
                        .to_string();
                    }
                    let mut tracker = self.cost_tracker.lock().unwrap();
                    tracker.check(&self.runs_dir, name, session_value, cwd, harness);
                    stop_now = tracker.take_stop();
                    drop(tracker);
                    let quiet = now.saturating_sub(*self.last_progress.lock().unwrap());
                    // R8: a Lead waiting on a live Peer, or on a thread that
                    // moved inside the quiet window, is not stalled. The
                    // workflow already honours `lead_waiting` in its idle and
                    // stall branches; the bus fills it in here, and only when
                    // one of those branches could fire.
                    if !*lead_waiting && err.is_empty() {
                        if let Some(window_ms) = peer_wait_window(
                            quiet,
                            now.saturating_sub(self.start_sec),
                            status.as_str(),
                            self.stall_sec,
                        ) {
                            if let Some(why) = peer_waiting(
                                &self.runs_dir,
                                name,
                                crate::bus::now_ms(),
                                window_ms,
                                &Herdr::new(),
                            ) {
                                *lead_waiting = true;
                                eprintln!("WATCHDOG waiting | name={name} quiet={quiet}s | {why}");
                            }
                        }
                    }
                    let diff = self.last_diff.lock().unwrap().clone();
                    let diffstat = if diff.is_empty() {
                        "clean".to_string()
                    } else {
                        diff.chars().take(8).collect()
                    };
                    *self.last_wait.lock().unwrap() = Some(WaitSnapshot {
                        status: if err.is_empty() {
                            status.clone()
                        } else {
                            format!("{status} ({err})")
                        },
                        diffstat: diffstat.clone(),
                    });
                    let mut lh = self.last_heartbeat.lock().unwrap();
                    if now.saturating_sub(*lh) >= self.heartbeat_sec {
                        *lh = now;
                        let elapsed = now.saturating_sub(self.start_sec);
                        eprintln!(
                            "WATCHDOG heartbeat | name={name} elapsed={elapsed}s status={status} quiet={quiet}s | diff={diffstat}"
                        );
                    }
                }
                if stop_now {
                    // on_cost_cap = "kill" (issue #48): interrupt once, then end
                    // the wait as blocked so the CoS decides; pane, worktree and
                    // journal stay for `--resume`.
                    match crate::worker::stop::interrupt_worker(name, &self.inner.herdr) {
                        Ok(line) => eprintln!("WATCHDOG cost stop | {line}"),
                        Err((_, e)) => eprint!("WATCHDOG cost stop failed | name={name} | {e}"),
                    }
                    *self.cost_stopped.lock().unwrap() = true;
                    if let Ok(ActivityResult::Herdr(
                        crate::engine::activity::HerdrResult::AgentWait { status, .. },
                    )) = &mut res
                    {
                        *status = "blocked".to_string();
                    }
                }
                res
            }
            ActivityRequest::Git(crate::engine::activity::GitRequest::DiffHash { .. }) => {
                if let Ok(ActivityResult::Git(crate::engine::activity::GitResult::DiffHash {
                    hash,
                    stat,
                })) = &res
                {
                    let mut prev = self.last_diff.lock().unwrap();
                    if *prev != *hash {
                        *prev = hash.clone();
                        *self.last_progress.lock().unwrap() = now_sec();
                    }
                    let _ = stat;
                }
                res
            }
            ActivityRequest::Herdr(HerdrRequest::Prompt { task, .. }) => {
                if task.starts_with("Time check: you are past") {
                    eprintln!(
                        "WATCHDOG nudge sent (once, at {}% of {}s budget)",
                        self.nudge_pct, self.time_budget_sec
                    );
                }
                res
            }
            _ => res,
        }
    }
}

/// `prompt_worker_once`, plus provider-error retry (#177): a turn that ends
/// idle/stalled (exit 3) or blocked (exit 5) without a report is classified
/// from the pane. A transient provider error is resumed after a capped
/// exponential backoff; a non-retryable one, or the retry cap, escalates with
/// exit 7. Every attempt and escalation is journaled on `worker:<name>`.
pub fn prompt_worker<G: GitOps>(
    name: &str,
    task: Option<&str>,
    resume: bool,
    extend: Option<u64>,
    git: &G,
    herdr: &Herdr,
) -> Result<(), (i32, String)> {
    let mut ended = retry::Ended::NotLanded;
    // A harness error record older than this moment belongs to an earlier turn.
    let turn_start = now_sec();
    let mut res = prompt_worker_once(name, task, resume, extend, git, herdr, &mut ended);
    let runs_dir = default_runs_dir();
    let mut attempt: u32 = 0;
    while let Err((code, _)) = &res {
        let code = *code;
        if !matches!(code, 3 | 5) || ended != retry::Ended::Watched {
            break;
        }
        let pane = herdr
            .agent_read(name, "recent-unwrapped", 80)
            .unwrap_or_default();
        let worker = retry::Worker {
            name,
            runs_dir: &runs_dir,
            harness: retry::harness_of(name),
            since: turn_start,
        };
        let Some(decision) = retry::decide(code, ended, &pane, attempt > 0, &worker) else {
            break;
        };
        let (reason, retryable) = match decision.error {
            retry::ProviderError::Retryable(r) => (r, true),
            retry::ProviderError::Fatal(r) => (r, false),
        };
        // A harness that already printed its terminal retry line gets one
        // short quaqua retry, not a second full ladder (#196).
        let cap = retry::retry_cap(&decision);
        attempt += 1;
        if !retryable || attempt > cap {
            let attempts = attempt - 1;
            retry::journal(
                &runs_dir,
                name,
                "provider_escalate",
                serde_json::json!({ "reason": reason, "retryable": retryable, "retries": attempts, "harness_exhausted": decision.exhausted, "exit": code }),
            );
            let _ = std::fs::remove_file(WorkerMetadata::provider_retry_path(&runs_dir, name));
            eprintln!(
                "WATCHDOG exit={} provider error ({reason}, {}) | name={name} retries={attempts} harness_exhausted={}",
                retry::EXIT_PROVIDER,
                if retryable {
                    "retry cap reached"
                } else {
                    "non-retryable"
                },
                decision.exhausted
            );
            return Err((retry::EXIT_PROVIDER, String::new()));
        }
        let delay_ms = retry::backoff_ms(attempt);
        retry::journal(
            &runs_dir,
            name,
            "provider_retry",
            serde_json::json!({ "attempt": attempt, "reason": reason, "delay_ms": delay_ms, "harness_exhausted": decision.exhausted }),
        );
        let _ = WorkerMetadata::record_provider_retry(&runs_dir, name, now_sec());
        eprintln!(
            "WATCHDOG provider-retry {attempt}/{cap} ({reason}) in {delay_ms}ms | name={name}"
        );
        std::thread::sleep(Duration::from_millis(delay_ms));
        let _ = herdr.agent_prompt_nowait(name, "continue");
        res = prompt_worker_once(name, None, true, None, git, herdr, &mut ended);
    }
    if attempt > 0 && res.is_ok() {
        let _ = std::fs::remove_file(WorkerMetadata::provider_retry_path(&runs_dir, name));
    }
    res
}

/// CLI entry: [`prompt_worker`] run in a session of its own, so the wait and
/// the report delivery outlive the coordinator that launched it. This process
/// only follows the core and mirrors its output and exit code.
pub fn prompt_worker_cli<G: GitOps>(
    name: &str,
    task: Option<&str>,
    resume: bool,
    extend: Option<u64>,
    git: &G,
    herdr: &Herdr,
) -> Result<(), (i32, String)> {
    if std::env::var_os(detach::ENV).is_none() {
        // A resume joins a core that is still running instead of racing it.
        match detach::run(&default_runs_dir(), name, resume) {
            Ok(0) => return Ok(()),
            Ok(code) => return Err((code, String::new())),
            // Could not detach: wait in this process, as before.
            Err(e) => eprintln!("WATCHDOG detach-failed | name={name} {e} — waiting in-process"),
        }
        return prompt_worker(name, task, resume, extend, git, herdr);
    }
    detach::core_begin(&default_runs_dir(), name);
    let res = prompt_worker(name, task, resume, extend, git, herdr);
    detach::core_end(&res);
    res
}

fn prompt_worker_once<G: GitOps>(
    name: &str,
    task: Option<&str>,
    resume: bool,
    extend: Option<u64>,
    git: &G,
    herdr: &Herdr,
    ended: &mut retry::Ended,
) -> Result<(), (i32, String)> {
    // A resume watches a turn whose brief already landed; a fresh task only
    // after the land check below.
    *ended = if task.is_none() {
        retry::Ended::Watched
    } else {
        retry::Ended::NotLanded
    };
    validate_env_budget()?;

    let runs_dir = default_runs_dir();
    let _ = std::fs::create_dir_all(&runs_dir);

    let target_owner = std::fs::read_to_string(runs_dir.join(format!("{name}.owner")))
        .ok()
        .map(|s| s.trim().to_string());
    let target_role = if name.ends_with("-lead") {
        "lead"
    } else {
        "peer"
    };
    if !resume {
        let cfg = crate::config::load_team_config().ok();
        crate::layout::gate(
            None,
            Some(herdr),
            crate::layout::Verb::Prompt,
            target_role,
            Some(name),
            crate::layout::SpawnOrigin::Cli,
            target_owner.as_deref(),
            None,
            &runs_dir,
            cfg.as_ref(),
        )?;
    }

    let out = runs_dir.join(format!("{name}.md"));
    let submit_log = WorkerMetadata::submit_log_path(&runs_dir, name);
    let cost_budget: Option<f64> = resolve_cost_budget(task.unwrap_or(""));

    let max: u64 = std::env::var("PROMPT_WORKER_MAX_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7200);

    let poll_sec: u64 = std::env::var("PROMPT_WORKER_POLL_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60)
        .max(1);
    let stall_sec: u64 = std::env::var("PROMPT_WORKER_STALL_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let idle_sec: u64 = std::env::var("PROMPT_WORKER_IDLE_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    let idle_fallback_sec: u64 = std::env::var("PROMPT_WORKER_IDLE_FALLBACK_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300.max(idle_sec));
    let gone_sec: u64 = std::env::var("PROMPT_WORKER_AGENT_GONE_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let nudge_pct: u64 = std::env::var("PROMPT_WORKER_NUDGE_PCT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(80);
    let heartbeat_sec: u64 = std::env::var("PROMPT_WORKER_HEARTBEAT_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let land_sec: u64 = 0;

    let expect_verdict = WorkerMetadata::has_review(&runs_dir, name);

    let mut config = WatchdogConfig {
        poll_sec,
        stall_sec,
        idle_sec,
        idle_fallback_sec,
        gone_sec,
        nudge_pct,
        hard_cap_sec: max,
        is_fallback_idle: false,
        expect_verdict,
        // Read from the pin written for the prompt this session belongs to. A
        // fresh run rewrites the pin below and refreshes this field; resume and
        // `--extend` keep the pin the worker was given.
        playbook: playbook_check::PlaybookPin::read(&runs_dir, name),
        land_sec,
    };

    let db_path = runs_dir.join("quaqua.db");
    let mut journal = SqliteJournal::open(&db_path).map_err(|e| {
        (
            1,
            format!(
                "error: failed to open journal at {}: {e}\n",
                db_path.display()
            ),
        )
    })?;

    // Probe existing sessions worker:<name>#<k>
    let mut k_max = 0u64;
    let mut last_snap: Option<SingleWorkerState> = None;
    let mut last_session_is_terminal = true;

    for k in 1.. {
        let wf_id = format!("worker:{name}#{k}");
        let events = journal.load_events(&wf_id, 1).unwrap_or_default();
        let snap = journal.load_latest_snapshot(&wf_id).ok().flatten();
        if events.is_empty() && snap.is_none() {
            break;
        }
        k_max = k;
        let is_terminal = events
            .iter()
            .any(|e| e.kind == "workflow_completed" || e.kind == "workflow_failed");
        last_session_is_terminal = is_terminal;
        if let Some(s) = snap {
            if let Ok(state) = serde_json::from_str::<SingleWorkerState>(&s.state_json) {
                last_snap = Some(state);
            }
        }
    }

    if k_max == 0 {
        let base_id = format!("worker:{name}");
        if let Ok(Some(s)) = journal.load_latest_snapshot(&base_id) {
            if let Ok(state) = serde_json::from_str::<SingleWorkerState>(&s.state_json) {
                last_snap = Some(state);
            }
        }
    }

    let fallback_kind = crate::config::load_team_config()
        .ok()
        .and_then(|cfg| crate::config::kind_of_worker(&cfg, name).map(|s| s.to_string()))
        .or_else(|| {
            crate::config::load_team_config()
                .ok()
                .map(|c| c.strategy.default_kind)
        })
        .unwrap_or_default();

    let fallback_repo = {
        // A fresh prompt needs a provably live worker: one failed lookup is
        // unknown, not down (issue #69). `--resume` keeps the watchdog's own
        // gone/exit-3 handling.
        let get_info = match liveness(herdr, name) {
            Liveness::Alive(body) => body,
            Liveness::Down if !resume => {
                return Err((1, format!("error: no live worker '{name}' — spawn it with `quaqua worker spawn`\n")))
            }
            Liveness::Unknown(why) if !resume => {
                return Err((
                    1,
                    format!("error: cannot determine whether worker '{name}' is live ({why}); not prompting\n"),
                ))
            }
            _ => String::new(),
        };
        let mut cwd = String::new();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&get_info) {
            if let Some(agent) = json.get("result").and_then(|r| r.get("agent")) {
                cwd = agent
                    .get("cwd")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
            }
        }
        if !cwd.is_empty() {
            cwd
        } else {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        }
    };

    enum ExecAction {
        Run {
            wf_id: String,
            state: Box<SingleWorkerState>,
            budget: u64,
        },
        Resume {
            wf_id: String,
            budget: u64,
        },
    }

    let exec_action: ExecAction;

    if !resume {
        let task_str = task.unwrap_or("");
        let budget = resolve_budget_sec(task_str).0;

        if out.exists() {
            let stale_path = format!("{}.stale.{}", out.display(), now_sec());
            let _ = std::fs::rename(&out, &stale_path);
            eprintln!(
                "archived pre-existing {} — a new task needs a fresh report",
                out.display()
            );
        }
        let _ = std::fs::remove_file(WorkerMetadata::provider_retry_path(&runs_dir, name));

        // § S4: a new task starts with no playbook pin. Dropping it before the
        // prompt means a submission that never lands (or a pin write that
        // fails) leaves no file for S5 to check against this task's report.
        brief::pin_playbook(&runs_dir, name, None);

        // Pre-prompt grid check
        if std::env::var("GRID_SKIP").as_deref() != Ok("1")
            || (std::env::var("PIPELINE_TASK").is_err()
                && std::env::var("QUAQUA_TEST").as_deref() != Ok("1"))
        {
            let grid_tab =
                std::fs::read_to_string(runs_dir.join(format!("{name}.grid"))).unwrap_or_default();
            let tab = {
                let t = grid_tab.trim();
                if t.is_empty() {
                    "workers"
                } else {
                    t
                }
            };
            if let Err(e) = crate::grid::run_grid_check(tab, Some(name), None) {
                return Err((1, format!("error: grid check failed for {name} ({e}); arrange panes manually before prompting\n")));
            }
        }

        // § S4: warn on a brief that names no playbook or no context, then
        // append the named playbook's Worker steps + cited leaves to the prompt.
        brief::warn_brief_headers(task_str);
        let owner_str = crate::state::WorkerMetadata::load_owner(&runs_dir, name);
        let owner = if owner_str.is_empty() {
            None
        } else {
            Some(owner_str.as_str())
        };
        brief::warn_lead_routing(task_str, owner, &runs_dir);
        let injection = brief::prepare_injection_in_workspace(task_str)?;

        // Synchronous land check: herdr's own "the prompt reached the turn"
        // signal. A stalled submission gets exactly one enter + re-wait.
        let prompt_body =
            brief::compose_prompt_body(name, task_str, injection.as_ref().map(|i| i.text.as_str()));
        let landed =
            match herdr.agent_prompt_wait(name, &prompt_body, &["working", "blocked"], 10_000) {
                Ok(out) => {
                    let (ok, status, code) = parse_submit_output(&out);
                    let _ = std::fs::write(
                        &submit_log,
                        String::from_utf8_lossy(&out.stdout).as_bytes(),
                    );
                    if ok && (status == "working" || status == "blocked" || status.is_empty()) {
                        // An empty successful payload is an older herdr without
                        // the land-check fields: the prompt was accepted.
                        true
                    } else if code == "agent_blocked" {
                        return Err((5, String::new()));
                    } else {
                        false
                    }
                }
                Err(_) => false,
            };
        if !landed {
            eprintln!(
                "WATCHDOG prompt-not-landed | name={name} — sent enter once (input may be typed but unsubmitted)"
            );
            let _ = herdr.agent_send_keys(name, "enter");
            let recovered = match herdr.agent_wait(name, &["working", "blocked"], 10_000) {
                Ok(out) => {
                    let (ok, status, _) = parse_submit_output(&out);
                    ok && (status == "working" || status == "blocked")
                }
                Err(_) => false,
            };
            if !recovered {
                eprintln!("WATCHDOG exit=3 prompt not landed | name={name}");
                return Err((3, String::new()));
            }
        }

        // The worker only got the steps if the prompt landed, so the pin
        // (`runs/.<name>.playbook`) is written here, after the land check.
        brief::pin_playbook(&runs_dir, name, injection.as_ref());
        *ended = retry::Ended::Watched;
        // § S8: this session checks the report against the pin it was given.
        config.playbook = playbook_check::PlaybookPin::read(&runs_dir, name);

        let next_k = k_max + 1;
        let wf_id = format!("worker:{name}#{next_k}");
        let repo = fallback_repo.clone();
        let kind = fallback_kind.clone();
        let input = SingleWorkerInput::new(name, kind, repo, task_str, budget, cost_budget);
        let state = SingleWorkerState::new(input)
            .with_report_path(&out)
            .with_config(config.clone());
        exec_action = ExecAction::Run {
            wf_id,
            state: Box::new(state),
            budget,
        };
    } else if let Some(ext) = extend {
        let next_k = k_max + 1;
        let wf_id = format!("worker:{name}#{next_k}");
        let (
            repo,
            kind,
            brief,
            old_budget,
            prev_elapsed,
            overrun_granted,
            extension_total,
            nudged,
            last_progress,
        ) = if let Some(s) = &last_snap {
            (
                s.input.repo.clone(),
                s.input.kind.clone(),
                s.input.brief.clone(),
                s.input.time_budget_sec,
                s.prev_elapsed_ms / 1000,
                s.overrun_granted,
                s.extension_total_ms / 1000,
                s.nudged,
                s.last_progress_ms / 1000,
            )
        } else {
            (
                fallback_repo.clone(),
                fallback_kind.clone(),
                String::new(),
                resolve_budget_sec("").0,
                0,
                false,
                0,
                false,
                0,
            )
        };
        let new_budget = old_budget + ext;
        eprintln!("WATCHDOG extend +{ext}s (allowance {new_budget}s, cap {max}s)");
        let input = SingleWorkerInput::new(name, kind, repo, brief, new_budget, cost_budget);
        let state = SingleWorkerState::new(input)
            .with_report_path(&out)
            .with_config(config.clone())
            .resumed(
                prev_elapsed,
                overrun_granted,
                extension_total,
                nudged,
                last_progress,
            );
        exec_action = ExecAction::Run {
            wf_id,
            state: Box::new(state),
            budget: new_budget,
        };
    } else {
        if k_max > 0 && !last_session_is_terminal {
            let wf_id = format!("worker:{name}#{k_max}");
            let budget = last_snap
                .as_ref()
                .map(|s| s.input.time_budget_sec)
                .unwrap_or_else(|| resolve_budget_sec("").0);
            exec_action = ExecAction::Resume { wf_id, budget };
        } else {
            let next_k = k_max + 1;
            let wf_id = format!("worker:{name}#{next_k}");
            let (
                repo,
                kind,
                brief,
                budget,
                prev_elapsed,
                overrun_granted,
                extension_total,
                nudged,
                last_progress,
            ) = if let Some(s) = &last_snap {
                (
                    s.input.repo.clone(),
                    s.input.kind.clone(),
                    s.input.brief.clone(),
                    s.input.time_budget_sec,
                    s.prev_elapsed_ms / 1000,
                    s.overrun_granted,
                    s.extension_total_ms / 1000,
                    s.nudged,
                    s.last_progress_ms / 1000,
                )
            } else {
                (
                    fallback_repo.clone(),
                    fallback_kind.clone(),
                    String::new(),
                    resolve_budget_sec("").0,
                    0,
                    false,
                    0,
                    false,
                    0,
                )
            };
            let input = SingleWorkerInput::new(name, kind, repo, brief, budget, cost_budget);
            let state = SingleWorkerState::new(input)
                .with_report_path(&out)
                .with_config(config.clone())
                .resumed(
                    prev_elapsed,
                    overrun_granted,
                    extension_total,
                    nudged,
                    last_progress,
                );
            exec_action = ExecAction::Run {
                wf_id,
                state: Box::new(state),
                budget,
            };
        }
    }

    let (active_wf_id, active_budget, repo_dir) = match &exec_action {
        ExecAction::Run {
            wf_id,
            state,
            budget,
        } => (wf_id.clone(), *budget, PathBuf::from(&state.input.repo)),
        ExecAction::Resume { wf_id, budget } => {
            (wf_id.clone(), *budget, PathBuf::from(&fallback_repo))
        }
    };

    eprintln!(
        "waiting for {} (poll {}s, stall {}s, idle {}s, budget {}s, cap {}s)",
        out.display(),
        config.poll_sec,
        config.stall_sec,
        config.idle_sec,
        active_budget,
        max
    );

    let team_cfg = crate::config::load_team_config().ok();
    let kill_on_cap = team_cfg
        .as_ref()
        .and_then(|cfg| {
            let kind = crate::config::kind_of_worker(cfg, name)?.to_string();
            let kill = cfg
                .kinds
                .get(&kind)
                .is_some_and(|k| k.on_cost_cap == crate::config::COST_CAP_KILL);
            // A kill kind without a stop key (own or fallback) cannot be
            // interrupted: warn instead of claiming an interrupt.
            match crate::config::kill_cap_supported(cfg, &kind) {
                Err(e) if kill => {
                    eprintln!("WATCHDOG on_cost_cap=kill ignored: {e}");
                    Some(false)
                }
                _ => Some(kill),
            }
        })
        .unwrap_or(false);
    let cost_buffer = team_cfg.map_or(0.2, |cfg| cfg.timers.cost_buffer);
    let start_sec = now_sec();
    let sys_acts = SystemActivities::with_repo_dir(&repo_dir);
    let prompt_acts = PromptActivities {
        inner: sys_acts,
        runs_dir: runs_dir.clone(),
        time_budget_sec: active_budget,
        stall_sec: config.stall_sec,
        nudge_pct,
        heartbeat_sec,
        start_sec,
        cost_tracker: Mutex::new(
            CostTracker::new(&runs_dir, name, cost_budget)
                .with_kill(kill_on_cap)
                .with_buffer(cost_buffer),
        ),
        cost_stopped: Mutex::new(false),
        last_wait: Mutex::new(None),
        last_progress: Mutex::new(start_sec),
        last_diff: Mutex::new(String::new()),
        last_heartbeat: Mutex::new(start_sec),
    };
    let mut acts = prompt_acts;
    let mut clock = PromptClock::new();
    let workflow = SingleWorkerWorkflow::new();

    let outcome = match exec_action {
        ExecAction::Run { wf_id, state, .. } => crate::engine::runner::run(
            &workflow,
            &wf_id,
            *state,
            &mut journal,
            &mut acts,
            &mut clock,
        ),
        ExecAction::Resume { wf_id, .. } => crate::engine::runner::resume(
            &workflow,
            &wf_id,
            &mut journal,
            &mut acts,
            &mut clock,
            Vec::new(),
        ),
    };

    let final_wait = acts.last_wait.lock().unwrap().clone().unwrap_or_default();
    let change_ts = *acts.last_progress.lock().unwrap();

    let watchdog_exit = |code: i32, reason: &str| -> (i32, String) {
        let now = now_sec();
        let status = if final_wait.status.is_empty() {
            "unknown"
        } else {
            final_wait.status.as_str()
        };
        let quiet = now.saturating_sub(change_ts);
        let diffstat = if final_wait.diffstat.is_empty() {
            "n/a"
        } else {
            final_wait.diffstat.as_str()
        };
        let elapsed = now.saturating_sub(start_sec);
        eprintln!(
            "WATCHDOG exit={code} {reason} | name={name} elapsed={elapsed}s status={status} quiet={quiet}s | diff={diffstat}"
        );
        (code, String::new())
    };
    let record_bus_event = |event_type: &str,
                            incident_key: Option<String>,
                            payload: serde_json::Value| {
        let owner_str = crate::state::WorkerMetadata::load_owner(&runs_dir, name);
        let owner = if owner_str.is_empty() {
            None
        } else {
            Some(owner_str)
        };
        let project = repo_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from);
        let thread_id = format!("quaqua/{name}");
        let mut pl = match payload {
            serde_json::Value::Object(m) => m,
            other => {
                let mut m = serde_json::Map::new();
                if other != serde_json::Value::Null {
                    m.insert("data".to_string(), other);
                }
                m
            }
        };
        if let Some(o) = &owner {
            pl.insert("owner".to_string(), serde_json::Value::String(o.clone()));
        }
        if let Some(p) = project {
            pl.insert("project".to_string(), serde_json::Value::String(p));
        }
        let summary = format!("{event_type} {name}");
        let mut trigger =
            crate::bus::NewTrigger::new("watchdog", event_type, "watchdog", &thread_id, summary)
                .with_payload_json(serde_json::Value::Object(pl).to_string());
        if let Some(o) = owner {
            trigger = trigger.with_to_name(o);
        }
        if let Some(k) = incident_key {
            trigger = trigger.with_dedupe_key(k);
        }
        crate::bus::record_trigger_safely_in_runs(&runs_dir, &trigger);
    };

    let single_output = match outcome {
        Ok(RunnerOutcome::Completed(o)) => o,
        Ok(RunnerOutcome::Blocked(reason)) => {
            let r = format!("workflow blocked: {reason}");
            record_bus_event(
                "stalled",
                Some(format!("stall:{name}:{start_sec}")),
                serde_json::json!({ "reason": &r }),
            );
            return Err(watchdog_exit(3, &r));
        }
        Ok(RunnerOutcome::Failed { code, reason }) => {
            let exit_code = if code != 0 { code } else { 3 };
            let r = format!("workflow failed: {reason}");
            record_bus_event(
                "stalled",
                Some(format!("stall:{name}:{start_sec}")),
                serde_json::json!({ "reason": &r }),
            );
            return Err(watchdog_exit(exit_code, &r));
        }
        Err(e) => {
            let r = format!("runner error: {e}");
            record_bus_event(
                "stalled",
                Some(format!("stall:{name}:{start_sec}")),
                serde_json::json!({ "reason": &r }),
            );
            return Err(watchdog_exit(3, &r));
        }
    };

    // Check if grace or autoext was granted during this execution
    if let Ok(Some(snap)) = journal.load_latest_snapshot(&active_wf_id) {
        if let Ok(state) = serde_json::from_str::<SingleWorkerState>(&snap.state_json) {
            let prior_grace = last_snap
                .as_ref()
                .map(|s| s.overrun_granted)
                .unwrap_or(false);
            if state.overrun_granted && !prior_grace {
                let grace_sec = (active_budget / 4).max(1);
                eprintln!("WATCHDOG grace +{grace_sec}s (progressing)");
            }
            let prior_autoext = last_snap
                .as_ref()
                .map(|s| s.extension_total_ms / 1000)
                .unwrap_or(0);
            let cur_autoext = state.extension_total_ms / 1000;
            if cur_autoext > prior_autoext {
                let diff = cur_autoext - prior_autoext;
                let max_autoext = active_budget / 2;
                eprintln!("WATCHDOG auto-extend +{diff}s (diff progressing, cap {max}s, total +{cur_autoext}s/+{max_autoext}s)");
            }
        }
    }

    match single_output.exit {
        0 => {
            let report_content = std::fs::read_to_string(&out).unwrap_or_default();
            if let Err(missing) = validate_report_content(&report_content, expect_verdict) {
                eprintln!(
                    "WATCHDOG malformed-report | name={name} missing={missing} report={}",
                    out.display()
                );
                record_bus_event(
                    "malformed",
                    Some(format!("malformed:{name}:{start_sec}")),
                    serde_json::json!({ "missing": missing }),
                );
                return Err((6, String::new()));
            }
            if let Err(why) = readonly::check(&runs_dir, name) {
                eprintln!(
                    "WATCHDOG malformed-report | name={name} missing=read-only worktree ({why}) report={}",
                    out.display()
                );
                record_bus_event(
                    "malformed",
                    Some(format!("malformed:{name}:{start_sec}")),
                    serde_json::json!({ "readonly": why }),
                );
                return Err((6, String::new()));
            }

            // § S5/S8: a builder report must account for the steps pinned in
            // `runs/.<name>.playbook`. A gap is a report the worker has to fix:
            // exit 6, like a malformed report, so `--resume` re-checks it once
            // the report is rewritten. Reviewer reports carry a `Verdict:` line
            // instead and are skipped. This block is shared by a fresh run and
            // by `--resume`, so both paths check.
            if let Some(gap) = playbook_check::record_playbook_gaps(
                &mut journal,
                &runs_dir,
                name,
                &report_content,
                expect_verdict,
            ) {
                eprintln!(
                    "WATCHDOG playbook-gap | name={name} {} report={}",
                    gap.summary(),
                    out.display()
                );
                record_bus_event(
                    "malformed",
                    Some(format!("malformed:{name}:{start_sec}")),
                    serde_json::json!({ "playbook_gap": gap.summary() }),
                );
                return Err((6, String::new()));
            }

            let _ = verdict::record_verdict_on_acceptance(
                &runs_dir,
                name,
                &report_content,
                expect_verdict,
                git,
                Some(&repo_dir),
            );

            let read_marker = runs_dir.join(format!(".{name}.read"));
            let _ = std::fs::write(&read_marker, format!("{}\n", now_sec()));

            record_bus_event("report_written", None, serde_json::json!({}));
            println!("{}", out.display());
            Ok(())
        }
        2 => {
            let reason = format!("error: no {} after {}s", out.display(), max);
            record_bus_event(
                "budget_exceeded",
                Some(format!("budget_exceeded:{name}:{start_sec}")),
                serde_json::json!({ "reason": reason }),
            );
            Err(watchdog_exit(2, &reason))
        }
        3 => {
            let now = now_sec();
            let st = if final_wait.status.is_empty() {
                "unknown"
            } else {
                final_wait.status.as_str()
            };
            let quiet = now.saturating_sub(change_ts);
            let reason = if st.starts_with("gone") {
                format!("agent gone: status={st} for {quiet}s")
            } else if st == "idle" || st == "done" {
                format!("agent {st} for {quiet}s without run file")
            } else {
                format!("stall: no worktree diff or session write for {quiet}s")
            };
            record_bus_event(
                "stalled",
                Some(format!("stall:{name}:{start_sec}")),
                serde_json::json!({ "reason": reason }),
            );
            Err(watchdog_exit(3, &reason))
        }
        4 => {
            let reason = format!("time budget {}s exceeded", active_budget);
            record_bus_event(
                "budget_exceeded",
                Some(format!("budget_exceeded:{name}:{start_sec}")),
                serde_json::json!({ "reason": reason }),
            );
            crate::worker::cleanup::record_stop_telemetry(name, &repo_dir, herdr, &runs_dir);
            Err(watchdog_exit(4, &reason))
        }
        5 if *acts.cost_stopped.lock().unwrap() => {
            *ended = retry::Ended::CostCap;
            record_bus_event(
                "blocked",
                Some(format!("blocked:{name}:{start_sec}")),
                serde_json::json!({ "reason": "cost cap reached" }),
            );
            Err(watchdog_exit(
                5,
                "cost cap reached (on_cost_cap=kill): worker interrupted; decide, then --resume",
            ))
        }
        5 => {
            record_bus_event(
                "blocked",
                Some(format!("blocked:{name}:{start_sec}")),
                serde_json::json!({ "reason": "agent blocked" }),
            );
            Err(watchdog_exit(
                5,
                "agent blocked (question/permission pending)",
            ))
        }
        6 => {
            let report_content = std::fs::read_to_string(&out).unwrap_or_default();
            match validate_report_content(&report_content, expect_verdict) {
                Err(missing) => {
                    eprintln!(
                        "WATCHDOG malformed-report | name={name} missing={missing} report={}",
                        out.display()
                    );
                }
                Ok(()) => {
                    // The workflow rejected the report for a pinned playbook
                    // step it does not account for; journal the gap here.
                    match playbook_check::record_playbook_gaps(
                        &mut journal,
                        &runs_dir,
                        name,
                        &report_content,
                        expect_verdict,
                    ) {
                        Some(gap) => eprintln!(
                            "WATCHDOG playbook-gap | name={name} {} report={}",
                            gap.summary(),
                            out.display()
                        ),
                        None => eprintln!(
                            "WATCHDOG malformed-report | name={name} missing=unknown section report={}",
                            out.display()
                        ),
                    }
                }
            }
            record_bus_event(
                "malformed",
                Some(format!("malformed:{name}:{start_sec}")),
                serde_json::json!({}),
            );
            Err((6, String::new()))
        }
        other => Err((other, format!("unexpected exit code {other}\n"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::{lock_env, TestEnvVarGuard};
    use std::fs;

    #[test]
    fn test_worker_prompt_keeps_exit_code_for_unparseable_reviewer_verdict() {
        // `prompt_worker` resolves these from the process env; a test that
        // writes the same vars must not interleave with this run.
        let _env_lock = lock_env();

        let tmp = std::env::temp_dir().join(format!("quaqua-wp-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let _guard_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", tmp.to_str().unwrap());
        let _guard_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _guard_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _guard_poll = TestEnvVarGuard::set("PROMPT_WORKER_POLL_SEC", "1");
        let name = "suhuynh-testr1";
        WorkerMetadata::mark_review(&tmp, name).unwrap();

        let report_file = tmp.join(format!("{name}.md"));
        fs::write(
            &report_file,
            "# Review\nVerdict: not clean\n## Lessons\nnone\n",
        )
        .unwrap();

        let stub_herdr = tmp.join("herdr-stub");
        fs::write(
            &stub_herdr,
            r#"#!/bin/sh
case "$1" in
  agent)
    case "$2" in
      wait) echo '{"result": {"agent": {"agent_status": "idle", "state_change_seq": 1}}}'; exit 0 ;;
      get) echo '{"result": {"agent": {"name": "'"$3"'", "agent_status": "idle"}}}'; exit 0 ;;
      *) echo '{"result": {}}'; exit 0 ;;
    esac ;;
  *) echo '{"result": {}}'; exit 0 ;;
esac
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&stub_herdr).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&stub_herdr, perms).unwrap();
        let _guard_herdr = TestEnvVarGuard::set("HERDR_BIN_PATH", stub_herdr.to_str().unwrap());

        let herdr = Herdr {
            bin: stub_herdr.to_string_lossy().to_string(),
        };
        let git = crate::git::Git::new();

        let res = prompt_worker(name, None, true, None, &git, &herdr);
        assert_eq!(
            res,
            Ok(()),
            "worker prompt must keep exit 0 for unparseable reviewer verdict"
        );

        let db_path = tmp.join("quaqua.db");
        if let Ok(journal) = SqliteJournal::open(&db_path) {
            let records = journal.load_verdicts("quaqua/suhuynh-testr1", "").unwrap();
            assert!(
                records.is_empty(),
                "unparseable report must not record verdict"
            );
        }
        let bus = crate::bus::FarmBus::open(&db_path).expect("open bus");
        let events = bus.load_events(0).expect("load events");
        assert!(
            events
                .iter()
                .any(|e| e.event_type == "report_written" && e.subject.as_deref() == Some(name)),
            "exit 0 must record report_written event in triggers"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    /// A fresh `runs/.<name>.state` from the agent's hooks decides the wait:
    /// herdr's screen says `idle`, the hook says `blocked`, and the watchdog
    /// ends at exit 5 instead of idling to exit 3.
    #[test]
    fn test_prompt_exits_5_when_the_state_file_reports_blocked() {
        let _env_lock = lock_env();
        let tmp = temp_runs("hook-blocked");
        let stub = stub_herdr(&tmp);
        let _g_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", tmp.to_str().unwrap());
        let _g_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _g_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _g_poll = TestEnvVarGuard::set("PROMPT_WORKER_POLL_SEC", "1");
        let _g_herdr = TestEnvVarGuard::set("HERDR_BIN_PATH", stub.to_str().unwrap());
        fs::write(
            tmp.join(".sude-hook.state"),
            format!("blocked {}\n", now_sec()),
        )
        .unwrap();

        let herdr = Herdr {
            bin: stub.to_string_lossy().to_string(),
        };
        let res = prompt_worker(
            "sude-hook",
            None,
            false,
            None,
            &crate::git::Git::new(),
            &herdr,
        );

        assert_eq!(
            res,
            Err((5, String::new())),
            "a fresh blocked state file must end the wait as blocked"
        );
        let bus = crate::bus::FarmBus::open(&tmp.join("quaqua.db")).expect("open bus");
        let events = bus.load_events(0).expect("load events");
        assert!(
            events
                .iter()
                .any(|e| e.event_type == "blocked" && e.subject.as_deref() == Some("sude-hook")),
            "exit 5 must record a blocked event in triggers"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    /// A `team.toml` fixture naming the `sude` kind's harness explicitly. The
    /// provider-error cases read the omp adapter's pane shapes, so the ambient
    /// `$HOME/quaqua/team.toml` (which may name another harness for `sude`,
    /// e.g. `agy`, and classify nothing) must not decide what they exercise.
    fn provider_team_config(tmp: &std::path::Path) -> std::path::PathBuf {
        let path = tmp.join("team.toml");
        fs::write(
            &path,
            "schema_version = 2\n\n[kinds.sude]\nharness = \"omp\"\nmodel = \"test-model\"\n",
        )
        .unwrap();
        path
    }

    /// Stub herdr for #177: the agent sits blocked; `agent read` prints
    /// the prompts sent so far (`> <text>`, like a real echo), then `FAKE_PANE`;
    /// each `agent prompt` appends its text to `FAKE_RUNS/prompts`.
    fn provider_stub(tmp: &std::path::Path) -> std::path::PathBuf {
        let path = tmp.join("herdr-provider");
        fs::write(
            &path,
            r#"#!/bin/sh
case "$1 $2" in
  "agent read") [ -f "$FAKE_RUNS/prompts" ] && sed 's/^/> /' "$FAKE_RUNS/prompts"; printf '%s\n' "$FAKE_PANE" ;;
  "agent prompt") printf '%s\n' "$4" >> "$FAKE_RUNS/prompts"; echo '{"result": {}}' ;;
  "agent get") echo '{"result": {"agent": {"name": "'"$3"'", "agent_status": "blocked"}}}' ;;
  "agent wait") echo '{"result": {"agent": {"agent_status": "blocked", "state_change_seq": 1}}}' ;;
  *) echo '{"result": {}}' ;;
esac
exit 0
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    type ProviderCase = (
        Result<(), (i32, String)>,
        Vec<(String, serde_json::Value)>,
        usize,
    );

    /// Runs `--resume` on a blocked worker whose pane shows `pane`; returns
    /// the result, the journaled provider events, and the prompts sent.
    fn run_provider_case(tag: &str, pane: &str) -> ProviderCase {
        let tmp = temp_runs(tag);
        let team = provider_team_config(&tmp);
        let stub = provider_stub(&tmp);
        let _g_team = TestEnvVarGuard::set("QUAQUA_TEAM_TOML", team.to_str().unwrap());
        let _g_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", tmp.to_str().unwrap());
        let _g_fruns = TestEnvVarGuard::set("FAKE_RUNS", tmp.to_str().unwrap());
        let _g_pane = TestEnvVarGuard::set("FAKE_PANE", pane);
        let _g_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _g_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _g_poll = TestEnvVarGuard::set("PROMPT_WORKER_POLL_SEC", "1");
        let _g_base = TestEnvVarGuard::set("PROMPT_WORKER_RETRY_BASE_MS", "1");
        let _g_herdr = TestEnvVarGuard::set("HERDR_BIN_PATH", stub.to_str().unwrap());
        let name = format!("sude-{tag}");
        fs::write(
            tmp.join(format!(".{name}.state")),
            format!("blocked {}\n", now_sec() + 3600),
        )
        .unwrap();
        let herdr = Herdr {
            bin: stub.to_string_lossy().to_string(),
        };
        let res = prompt_worker(&name, None, true, None, &crate::git::Git::new(), &herdr);
        let journal = SqliteJournal::open(&tmp.join("quaqua.db")).unwrap();
        let events = journal
            .load_events(&format!("worker:{name}"), 1)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.kind.starts_with("provider_"))
            .map(|e| {
                (
                    e.kind.clone(),
                    serde_json::from_str(&e.payload_json).unwrap(),
                )
            })
            .collect();
        let prompts = fs::read_to_string(tmp.join("prompts"))
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == "continue")
            .count();
        let _ = fs::remove_dir_all(&tmp);
        (res, events, prompts)
    }

    /// #196: a harness that already printed its terminal retry line gets one
    /// short quaqua retry, not a second full ladder.
    #[test]
    fn test_prompt_caps_retries_after_a_harness_exhausted_line() {
        let _env_lock = lock_env();
        let (res, events, prompts) = run_provider_case(
            "exhausted",
            "Retry budget exhausted after 10 retries: 429 Rate limit reached",
        );
        assert_eq!(res, Err((7, String::new())));
        assert_eq!(prompts, 1, "the single retry resumes the turn once");
        let retries: Vec<_> = events
            .iter()
            .filter(|(k, _)| k == "provider_retry")
            .collect();
        assert_eq!(retries.len(), 1, "one short retry, not the full ladder");
        assert_eq!(retries[0].1["harness_exhausted"], true);
        let (kind, esc) = events.last().unwrap();
        assert_eq!(kind, "provider_escalate");
        assert_eq!(esc["harness_exhausted"], true);
        assert_eq!(esc["retries"], 1);
    }

    /// #177: an overloaded turn is resumed 4 times with doubling delays,
    /// each attempt journaled, then escalates with exit 7.
    #[test]
    fn test_prompt_retries_an_overloaded_turn_with_backoff_then_escalates() {
        let _env_lock = lock_env();
        let (res, events, prompts) = run_provider_case("overload", "Error: 529 Overloaded");
        assert_eq!(res, Err((7, String::new())));
        assert_eq!(prompts, 4, "each retry resumes the turn once");
        let retries: Vec<_> = events
            .iter()
            .filter(|(k, _)| k == "provider_retry")
            .collect();
        let delays: Vec<u64> = retries
            .iter()
            .map(|(_, p)| p["delay_ms"].as_u64().unwrap())
            .collect();
        assert_eq!(delays, vec![1, 2, 4, 8]);
        assert!(retries.iter().all(|(_, p)| p["reason"] == "overloaded"));
        let (kind, esc) = events.last().unwrap();
        assert_eq!(kind, "provider_escalate");
        assert_eq!(esc["retryable"], true);
        assert_eq!(esc["retries"], 4);
    }

    /// #177: a signed-out turn escalates at once — no retry, no resume prompt.
    #[test]
    fn test_prompt_escalates_a_signed_out_turn_without_retry() {
        let _env_lock = lock_env();
        let (res, events, prompts) = run_provider_case(
            "signout",
            "401 Incorrect API key provided: sk-fake. (type=invalid_request_error param=invalid_api_key)",
        );
        assert_eq!(res, Err((7, String::new())));
        assert_eq!(prompts, 0);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, "provider_escalate");
        assert_eq!(events[0].1["retryable"], false);
        assert_eq!(events[0].1["reason"], "signed-out");
    }

    /// A blocked turn with no provider error keeps exit 5 (meaning unchanged).
    #[test]
    fn test_prompt_keeps_exit_5_when_the_pane_shows_no_provider_error() {
        let _env_lock = lock_env();
        let (res, events, prompts) =
            run_provider_case("plainblk", "Do you want to proceed? 1. Yes");
        assert_eq!(res, Err((5, String::new())));
        assert!(events.is_empty());
        assert_eq!(prompts, 0);
    }

    /// A fresh, isolated runs dir for one prompt case.
    fn temp_runs(tag: &str) -> std::path::PathBuf {
        static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("quaqua-wp-{tag}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    /// Stub herdr: an idle agent. A fresh prompt writes `FAKE_REPORT` to
    /// `FAKE_RUNS/<name>.md`, which is what the agent does before it yields.
    fn stub_herdr(tmp: &std::path::Path) -> std::path::PathBuf {
        let path = tmp.join("herdr-stub");
        fs::write(
            &path,
            r#"#!/bin/sh
case "$1" in
  agent)
    case "$2" in
      prompt)
        if [ -n "$FAKE_REPORT" ]; then
          printf '%s\n' "$FAKE_REPORT" > "$FAKE_RUNS/$3.md"
        fi
        echo '{"result": {}}'; exit 0 ;;
      wait) echo '{"result": {"agent": {"agent_status": "idle", "state_change_seq": 1}}}'; exit 0 ;;
      get) echo '{"result": {"agent": {"name": "'"$3"'", "agent_status": "idle"}}}'; exit 0 ;;
      *) echo '{"result": {}}'; exit 0 ;;
    esac ;;
  *) echo '{"result": {}}'; exit 0 ;;
esac
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// `(playbooks, principles)` holding one two-step `feature` playbook.
    fn playbook_index(tmp: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let playbooks = tmp.join("playbooks");
        let principles = tmp.join("principles");
        fs::create_dir_all(&playbooks).unwrap();
        fs::create_dir_all(&principles).unwrap();
        fs::write(
            playbooks.join("feature.md"),
            "---\nname: feature\nwhen: test\n---\n\n## Orchestrator\n\n1. Orchestrate.\n\n## Worker\n\n1. First step.\n2. Second step.\n",
        )
        .unwrap();
        (playbooks, principles)
    }

    const PINNED_BRIEF: &str = "Goal: build it\nHard constraints: none\nCurrent design (open to challenge): x\nPlaybook: feature\nContext: none, test";

    /// `## Playbook` event kinds journaled for `session`.
    fn journaled_gaps(tmp: &std::path::Path, session: &str) -> Vec<String> {
        let journal = SqliteJournal::open(&tmp.join("quaqua.db")).unwrap();
        journal
            .load_events(session, 1)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.kind == "playbook_gap")
            .map(|e| e.kind)
            .collect()
    }

    #[test]
    fn test_prompt_exits_6_for_a_builder_report_without_its_pinned_playbook() {
        let _env_lock = lock_env();
        let tmp = temp_runs("gap");
        let (playbooks, principles) = playbook_index(&tmp);
        let stub = stub_herdr(&tmp);
        let _g_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", tmp.to_str().unwrap());
        let _g_playbooks =
            TestEnvVarGuard::set("QUAQUA_PLAYBOOKS_DIR", playbooks.to_str().unwrap());
        let _g_principles =
            TestEnvVarGuard::set("QUAQUA_PRINCIPLES_DIR", principles.to_str().unwrap());
        let _g_report = TestEnvVarGuard::set(
            "FAKE_REPORT",
            "Outcome: COMPLETE\n# Report\n\n## Lessons\nnone\n",
        );
        let _g_fake_runs = TestEnvVarGuard::set("FAKE_RUNS", tmp.to_str().unwrap());
        let _g_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _g_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _g_poll = TestEnvVarGuard::set("PROMPT_WORKER_POLL_SEC", "1");
        let _g_herdr = TestEnvVarGuard::set("HERDR_BIN_PATH", stub.to_str().unwrap());

        let herdr = Herdr {
            bin: stub.to_string_lossy().to_string(),
        };
        let res = prompt_worker(
            "sude-gap",
            Some(PINNED_BRIEF),
            false,
            None,
            &crate::git::Git::new(),
            &herdr,
        );

        assert_eq!(
            res,
            Err((6, String::new())),
            "a missing `## Playbook` block must reject the report"
        );
        let bus = crate::bus::FarmBus::open(&tmp.join("quaqua.db")).expect("open bus");
        let events = bus.load_events(0).expect("load events");
        assert!(
            events
                .iter()
                .any(|e| e.event_type == "malformed" && e.subject.as_deref() == Some("sude-gap")),
            "exit 6 must record malformed event in triggers"
        );

        assert!(
            tmp.join(".sude-gap.playbook").is_file(),
            "the prompt pinned the playbook the worker received"
        );
        assert_eq!(
            journaled_gaps(&tmp, "worker:sude-gap").len(),
            1,
            "the playbook_gap event is kept"
        );
        assert!(
            !tmp.join(".sude-gap.read").exists(),
            "a rejected report is not accepted"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_prompt_accepts_a_builder_report_that_accounts_for_every_step() {
        let _env_lock = lock_env();
        let tmp = temp_runs("pinned");
        let (playbooks, principles) = playbook_index(&tmp);
        let stub = stub_herdr(&tmp);
        let _g_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", tmp.to_str().unwrap());
        let _g_playbooks =
            TestEnvVarGuard::set("QUAQUA_PLAYBOOKS_DIR", playbooks.to_str().unwrap());
        let _g_principles =
            TestEnvVarGuard::set("QUAQUA_PRINCIPLES_DIR", principles.to_str().unwrap());
        let _g_report = TestEnvVarGuard::set(
            "FAKE_REPORT",
            "Outcome: COMPLETE\n# Report\n\n## Playbook\n1: done\n2: skip: unsupported on stub\n\n## Lessons\nnone\n",
        );
        let _g_fake_runs = TestEnvVarGuard::set("FAKE_RUNS", tmp.to_str().unwrap());
        let _g_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _g_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _g_poll = TestEnvVarGuard::set("PROMPT_WORKER_POLL_SEC", "1");
        let _g_herdr = TestEnvVarGuard::set("HERDR_BIN_PATH", stub.to_str().unwrap());

        let herdr = Herdr {
            bin: stub.to_string_lossy().to_string(),
        };
        let res = prompt_worker(
            "sude-pinned",
            Some(PINNED_BRIEF),
            false,
            None,
            &crate::git::Git::new(),
            &herdr,
        );

        assert_eq!(res, Ok(()), "every pinned step accounted for -> accepted");
        assert!(journaled_gaps(&tmp, "worker:sude-pinned").is_empty());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_prompt_ignores_the_pin_for_a_reviewer_report() {
        let _env_lock = lock_env();
        let tmp = temp_runs("reviewer-pin");
        let stub = stub_herdr(&tmp);
        let _g_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", tmp.to_str().unwrap());
        let _g_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _g_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _g_poll = TestEnvVarGuard::set("PROMPT_WORKER_POLL_SEC", "1");
        let _g_herdr = TestEnvVarGuard::set("HERDR_BIN_PATH", stub.to_str().unwrap());
        let name = "suhuynh-rpin1";
        WorkerMetadata::mark_review(&tmp, name).unwrap();
        fs::write(
            tmp.join(format!("{name}.md")),
            "# Review\n\nVerdict: clean\n\n## Lessons\nnone\n",
        )
        .unwrap();
        fs::write(
            tmp.join(format!(".{name}.playbook")),
            "name=feature\nids=1,2\n",
        )
        .unwrap();

        let herdr = Herdr {
            bin: stub.to_string_lossy().to_string(),
        };
        let res = prompt_worker(name, None, true, None, &crate::git::Git::new(), &herdr);

        assert_eq!(
            res,
            Ok(()),
            "reviewer reports are never checked against a playbook"
        );
        assert!(journaled_gaps(&tmp, &format!("worker:{name}")).is_empty());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_prompt_resume_accepts_the_report_after_the_playbook_block_is_added() {
        let _env_lock = lock_env();
        let tmp = temp_runs("gap-resume");
        let stub = stub_herdr(&tmp);
        let _g_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", tmp.to_str().unwrap());
        let _g_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _g_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _g_poll = TestEnvVarGuard::set("PROMPT_WORKER_POLL_SEC", "1");
        let _g_herdr = TestEnvVarGuard::set("HERDR_BIN_PATH", stub.to_str().unwrap());
        let name = "sude-gapfix";
        let report = tmp.join(format!("{name}.md"));
        fs::write(&report, "Outcome: COMPLETE\n# Report\n\n## Lessons\nnone\n").unwrap();
        fs::write(
            tmp.join(format!(".{name}.playbook")),
            "name=feature\nids=1,2\n",
        )
        .unwrap();

        let herdr = Herdr {
            bin: stub.to_string_lossy().to_string(),
        };
        let git = crate::git::Git::new();
        let first = prompt_worker(name, None, true, None, &git, &herdr);
        assert_eq!(first, Err((6, String::new())), "the gap rejects the report");
        assert_eq!(
            journaled_gaps(&tmp, &format!("worker:{name}")).len(),
            1,
            "the rejection journals one playbook_gap"
        );

        // The worker rewrites the report atomically, then the CoS resumes.
        let pending = tmp.join(format!(".{name}.md.tmp"));
        fs::write(
            &pending,
            "Outcome: COMPLETE\n# Report\n\n## Playbook\n1: done\n2: done\n\n## Lessons\nnone\n",
        )
        .unwrap();
        fs::rename(&pending, &report).unwrap();

        let second = prompt_worker(name, None, true, None, &git, &herdr);
        assert_eq!(second, Ok(()), "--resume accepts the fixed report");
        assert_eq!(
            journaled_gaps(&tmp, &format!("worker:{name}")).len(),
            1,
            "the accepted resume journals no second gap"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_watchdog_records_valid_json_payload_when_readonly_fails_with_special_chars() {
        let _env_lock = lock_env();
        let tmp = temp_runs("f2-readonly-json");
        let stub = stub_herdr(&tmp);
        let _g_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", tmp.to_str().unwrap());
        let _g_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _g_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _g_poll = TestEnvVarGuard::set("PROMPT_WORKER_POLL_SEC", "1");
        let _g_herdr = TestEnvVarGuard::set("HERDR_BIN_PATH", stub.to_str().unwrap());
        let name = "suhuynh-revtest";
        WorkerMetadata::mark_review(&tmp, name).unwrap();

        // Baseline pointing to a non-existent path containing double quotes and newlines:
        // Git error output will contain quotes/special characters.
        let bogus_wt = tmp.join("path\"with\"quotes\nand\\backslashes");
        let baseline_file = readonly::baseline_path(&tmp, name);
        fs::write(
            &baseline_file,
            format!("{}\nHEAD abc123\n", bogus_wt.display()),
        )
        .unwrap();

        let report_file = tmp.join(format!("{name}.md"));
        fs::write(&report_file, "# Review\nVerdict: clean\n## Lessons\nnone\n").unwrap();

        let herdr = Herdr {
            bin: stub.to_string_lossy().to_string(),
        };
        let git = crate::git::Git::new();

        let res = prompt_worker(name, None, true, None, &git, &herdr);
        assert_eq!(res, Err((6, String::new())));

        let bus = crate::bus::FarmBus::open(&tmp.join("quaqua.db")).expect("open bus");
        let events = bus.load_events(0).expect("load events");
        let malformed_ev = events
            .iter()
            .find(|e| e.event_type == "malformed" && e.subject.as_deref() == Some(name))
            .expect("malformed event recorded");

        let parsed: serde_json::Value = serde_json::from_str(&malformed_ev.payload_json)
            .expect("payload_json must be valid JSON even when error message contains special characters and newlines");
        assert!(parsed.get("readonly").is_some());
        let _ = fs::remove_dir_all(&tmp);
    }

    /// Issue #191: while the watchdog is blocked inside an activity, a second
    /// writer on the same `worker:<name>#k` session can commit a tick of its
    /// own, leaving the runner's in-memory cursor behind the journal head. The
    /// append must rebase onto the current head and land, not kill the
    /// watchdog with a sequence-conflict exit 3.
    #[test]
    fn test_prompt_rebases_a_stale_cursor_from_a_second_writer() {
        let _env_lock = lock_env();
        let tmp = temp_runs("seq-rebase");
        let name = "sude-rebase";
        let wf = format!("worker:{name}#1");
        let db_path = tmp.join("quaqua.db");
        fs::write(
            tmp.join(format!("{name}.md")),
            "Outcome: COMPLETE\n# Report\n\n## Lessons\nnone\n",
        )
        .unwrap();

        // Stub herdr for the watchdog's polls. On its first `agent` call it
        // signals the test's second writer and blocks until that writer has
        // committed, so the foreign tick lands inside the activity — the
        // 13:08 -> 13:09 shape of the incident, without a timing race.
        let race_req = tmp.join(".race-req");
        let race_done = tmp.join(".race-done");
        let stub = tmp.join("herdr-race-stub");
        fs::write(
            &stub,
            r#"#!/bin/sh
if [ "$1" = "agent" ] && [ -n "$RACE_REQ" ] && [ ! -f "$RACE_REQ" ]; then
  : > "$RACE_REQ"
  i=0
  while [ ! -f "$RACE_DONE" ] && [ "$i" -lt 300 ]; do
    sleep 0.01
    i=$((i+1))
  done
fi
case "$1" in
  agent)
    case "$2" in
      wait) echo '{"result": {"agent": {"agent_status": "idle", "state_change_seq": 1}}}'; exit 0 ;;
      get) echo '{"result": {"agent": {"name": "'"$3"'", "agent_status": "idle"}}}'; exit 0 ;;
      *) echo '{"result": {}}'; exit 0 ;;
    esac ;;
  *) echo '{"result": {}}'; exit 0 ;;
esac
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

        let _g_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", tmp.to_str().unwrap());
        let _g_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _g_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _g_poll = TestEnvVarGuard::set("PROMPT_WORKER_POLL_SEC", "1");
        let _g_herdr = TestEnvVarGuard::set("HERDR_BIN_PATH", stub.to_str().unwrap());
        let _g_req = TestEnvVarGuard::set("RACE_REQ", race_req.to_str().unwrap());
        let _g_done = TestEnvVarGuard::set("RACE_DONE", race_done.to_str().unwrap());

        // The second writer: a 12-event tick committed in-process through
        // rusqlite while the watchdog is parked inside its first herdr poll.
        let racer = {
            let db_path = db_path.clone();
            let wf = wf.clone();
            let (req, done) = (race_req.clone(), race_done.clone());
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while !req.exists() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert!(req.exists(), "the stub never reached its first agent wait");
                let conn = rusqlite::Connection::open(&db_path).expect("open the journal db");
                conn.busy_timeout(std::time::Duration::from_millis(5000))
                    .expect("set a busy timeout");
                conn.execute_batch("BEGIN IMMEDIATE;").expect("begin");
                for _ in 0..12 {
                    conn.execute(
                        "INSERT INTO events (workflow_id, seq, ts_ms, kind, payload_json) \
                         SELECT ?1, COALESCE(MAX(seq), 0) + 1, 1, 'other_writer', '{}' \
                         FROM events WHERE workflow_id = ?1;",
                        rusqlite::params![wf],
                    )
                    .expect("insert a second-writer event");
                }
                conn.execute_batch("COMMIT;").expect("commit");
                fs::write(&done, b"done").expect("signal the stub");
            })
        };

        let herdr = Herdr {
            bin: stub.to_string_lossy().to_string(),
        };
        let git = crate::git::Git::new();
        let res = prompt_worker(name, None, true, None, &git, &herdr);
        racer.join().expect("the second writer must finish");
        assert_eq!(
            res,
            Ok(()),
            "a stale cursor must be rebased, not end the watchdog at exit 3"
        );

        let journal = SqliteJournal::open(&db_path).unwrap();
        let events = journal.load_events(&wf, 1).unwrap();
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        let want: Vec<u64> = (1..=events.len() as u64).collect();
        assert_eq!(seqs, want, "the journal must stay one contiguous chain");
        assert_eq!(
            events.iter().filter(|e| e.kind == "other_writer").count(),
            12,
            "no second-writer event is lost"
        );
        // The append that lost the race was `wake_0`'s completion; the refused
        // insert rolled back, so the rebased event must land once, not twice.
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == "activity_completed" && e.payload_json.contains("wake_0"))
                .count(),
            1,
            "the rebased activity_completed must land exactly once"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == "workflow_completed")
                .count(),
            1,
            "the workflow completes exactly once"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_worker_prompt_runner_error_gives_non_zero_watchdog_exit() {
        let _env_lock = lock_env();

        let tmp = std::env::temp_dir().join(format!("quaqua-wp-runner-err-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let _guard_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", tmp.to_str().unwrap());
        let _guard_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _guard_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _guard_poll = TestEnvVarGuard::set("PROMPT_WORKER_POLL_SEC", "1");

        let name = "sude-runner-err";
        let stub_herdr = tmp.join("herdr-stub");
        fs::write(
            &stub_herdr,
            r#"#!/bin/sh
echo '{"result": {"agent": {"agent_status": "working"}}}'
exit 0
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&stub_herdr).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&stub_herdr, perms).unwrap();
        let _guard_herdr = TestEnvVarGuard::set("HERDR_BIN_PATH", stub_herdr.to_str().unwrap());

        let herdr = Herdr {
            bin: stub_herdr.to_string_lossy().to_string(),
        };
        let git = crate::git::Git::new();

        let db_path = tmp.join("quaqua.db");
        {
            let mut j = SqliteJournal::open(&db_path).unwrap();
            let snap = crate::engine::journal::SnapshotRecord::new(1, "{ corrupt json");
            j.append_snapshot(&format!("worker:{name}#1"), &snap)
                .unwrap();
        }

        // Calling resume on a worker with corrupt state triggers RunnerError::Serialization
        let res = prompt_worker(name, None, true, None, &git, &herdr);
        // Runner failure must return a non-zero WATCHDOG exit (code 3), never exit 0 without a report
        assert_eq!(
            res,
            Err((3, String::new())),
            "runner failure must return watchdog exit 3"
        );

        let _ = fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod check_brief_review_tests {
    use super::*;

    const BASE: &str = "Topology: solo — test\nPlaybook: none, test\nContext: none, test\nGoal: g\nHard constraints: h\nCurrent design (open to challenge): d\n";

    #[test]
    fn check_brief_review_fields_validate() {
        let dir = std::env::temp_dir().join(format!("qq-cbr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runs = &dir;
        assert!(do_check_brief_with_owner(BASE, None, runs).is_ok());
        let ok = format!("{BASE}Must fix: minor\nReview rounds: 2\n");
        assert!(do_check_brief_with_owner(&ok, None, runs).is_ok());
        let bad = format!("{BASE}Must fix: blocker\n");
        assert_eq!(
            do_check_brief_with_owner(&bad, None, runs).unwrap_err().0,
            1
        );
        let bad = format!("{BASE}Review rounds: 0\n");
        assert_eq!(
            do_check_brief_with_owner(&bad, None, runs).unwrap_err().0,
            1
        );
    }
}
