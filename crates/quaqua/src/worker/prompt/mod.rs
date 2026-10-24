//! Worker prompt & watchdog logic (`quaqua worker prompt <name> <task>`).

pub mod brief;
pub mod cost;
pub mod observer;
pub mod playbook_check;
pub mod readonly;
pub mod verdict;
pub mod watchdog;
pub mod workflow;

pub use crate::util::now_sec;
pub use cost::{brief_cost_budget, read_worker_cost, resolve_cost_budget};
pub use observer::{lead_waiting, report_ready};
pub use watchdog::{is_lessons_heading, nap, prepend_header, validate_report_content};

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

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
            } else if let Some(rem) = after_digits.strip_prefix('m') {
                Some(rem)
            } else {
                None
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
        if let Some(c) = json
            .pointer("/error/code")
            .and_then(|v| v.as_str())
        {
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

struct PromptActivities {
    inner: SystemActivities,
    runs_dir: PathBuf,
    time_budget_sec: u64,
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
                        ..
                    },
                )) = &res
                {
                    let now = now_sec();
                    let mut tracker = self.cost_tracker.lock().unwrap();
                    tracker.check(&self.runs_dir, name, session_value, cwd, harness);
                    stop_now = tracker.take_stop();
                    drop(tracker);
                    let quiet = now.saturating_sub(*self.last_progress.lock().unwrap());
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

pub fn prompt_worker<G: GitOps>(
    name: &str,
    task: Option<&str>,
    resume: bool,
    extend: Option<u64>,
    git: &G,
    herdr: &Herdr,
) -> Result<(), (i32, String)> {
    validate_env_budget()?;

    let runs_dir = default_runs_dir();
    let _ = std::fs::create_dir_all(&runs_dir);

    let target_owner = std::fs::read_to_string(runs_dir.join(format!("{name}.owner")))
        .ok()
        .map(|s| s.trim().to_string());
    let target_role = if name.ends_with("-lead") { "lead" } else { "peer" };
    crate::layout::shadow(
        None,
        Some(herdr),
        crate::layout::Verb::Prompt,
        target_role,
        Some(name),
        crate::layout::SpawnOrigin::Cli,
        target_owner.as_deref(),
        None,
        &runs_dir,
        None,
    );

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
            state: SingleWorkerState,
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
            state,
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
            state,
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
                state,
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
            cfg.kinds.get(&kind).map(|k| k.on_cost_cap == crate::config::COST_CAP_KILL)
        })
        .unwrap_or(false);
    let cost_buffer = team_cfg.map_or(0.2, |cfg| cfg.timers.cost_buffer);
    let start_sec = now_sec();
    let sys_acts = SystemActivities::with_repo_dir(&repo_dir);
    let prompt_acts = PromptActivities {
        inner: sys_acts,
        runs_dir: runs_dir.clone(),
        time_budget_sec: active_budget,
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
            state,
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

    let single_output = match outcome {
        Ok(RunnerOutcome::Completed(o)) => o,
        Ok(RunnerOutcome::Blocked(reason)) => {
            return Err((1, format!("workflow blocked: {reason}\n")))
        }
        Ok(RunnerOutcome::Failed { code, reason }) => {
            return Err((code, format!("workflow failed: {reason}\n")))
        }
        Err(e) => return Err((1, format!("runner error: {e}\n"))),
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
    let record_bus_event = |event_type: &str, incident_key: Option<String>, payload: serde_json::Value| {
        let owner_str = crate::state::WorkerMetadata::load_owner(&runs_dir, name);
        let owner = if owner_str.is_empty() { None } else { Some(owner_str) };
        let project = repo_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from);
        let mut ev = crate::bus::FarmEvent::new(
            event_type,
            Some("watchdog"),
            Some(name),
            owner,
            project,
        );
        if let Some(key) = incident_key {
            ev = ev.with_incident_key(key);
        }
        let payload_str = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
        if !payload_str.is_empty() && payload_str != "{}" {
            ev = ev.with_payload_json(payload_str);
        }
        crate::bus::record_event_safely_in_runs(&runs_dir, &ev);
    };


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
            Err(watchdog_exit(4, &reason))
        }
        5 if *acts.cost_stopped.lock().unwrap() => {
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
        let farm_events = bus.load_events(0).expect("load farm events");
        assert!(
            farm_events.iter().any(|e| e.event_type == "report_written" && e.subject.as_deref() == Some(name)),
            "exit 0 must record report_written event in farm_events"
        );


        let _ = fs::remove_dir_all(&tmp);
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
        let _g_report = TestEnvVarGuard::set("FAKE_REPORT", "# Report\n\n## Lessons\nnone\n");
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
        let farm_events = bus.load_events(0).expect("load farm events");
        assert!(
            farm_events.iter().any(|e| e.event_type == "malformed" && e.subject.as_deref() == Some("sude-gap")),
            "exit 6 must record malformed event in farm_events"
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
            "# Report\n\n## Playbook\n1: done\n2: skip: unsupported on stub\n\n## Lessons\nnone\n",
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
        fs::write(&report, "# Report\n\n## Lessons\nnone\n").unwrap();
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
            "# Report\n\n## Playbook\n1: done\n2: done\n\n## Lessons\nnone\n",
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
        fs::write(&baseline_file, format!("{}\nHEAD abc123\n", bogus_wt.display())).unwrap();

        let report_file = tmp.join(format!("{name}.md"));
        fs::write(
            &report_file,
            "# Review\nVerdict: clean\n## Lessons\nnone\n",
        )
        .unwrap();

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
}
