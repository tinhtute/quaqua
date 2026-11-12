//! SingleWorkerWorkflow implementation over the extended W0 engine contract.
//!
//! Evaluates worker watchdog rules deterministically:
//! - Exit 0: Success (report ready with stable mtime, non-empty content, and valid `## Lessons` or `Verdict:`)
//! - Exit 2: Hard timeout / cap exceeded
//! - Exit 3: Stall (no worktree diff and no harness session write for 600 s) /
//!   idle (120 s, 300 s fallback) / agent gone (60 s) / prompt never landed
//! - Exit 4: Time budget exceeded (after one +25% grace and bounded auto-extensions up to +50% total)
//! - Exit 5: Blocked on question / permission. A hook-reported `blocked` ends
//!   the wait at once; a screen-inferred one is a hypothesis (a non-modal choice
//!   banner has a dialog's footer shape) and must persist for a poll with no
//!   progress behind it.
//! - Exit 6: Malformed report (stable, non-empty report missing `## Lessons` or
//!   `Verdict:`) or a builder report that does not account for a pinned
//!   `## Playbook` step
//! - Exit 7 (set by `prompt_worker`, not this workflow): provider error
//!   escalated after retries, or non-retryable — see `retry.rs`
//!
//! Signals and contracts:
//! - One blocking `herdr agent wait` per wake; `timeout_ms == 0` is a plain `agent get`.
//! - Progress is glyph-free: `GitRequest::DiffHash` plus the harness session-store
//!   mtime (`SessionRequest::Mtime`). A harness with neither source gets the time
//!   budget only; the stall check is skipped.
//! - Report readiness is one `report_ready` check per wake (double-stat), then a read.
//! - Auto-extension keys exclusively to `GitRequest::DiffHash`.
//!
//! Determinism contract: No `std::fs`, `std::process`, or `std::net` is used.
//! All external observation and time progression occur through scheduled activities
//! and virtual time arithmetic.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::engine::activity::{
    ActivityRequest, ActivityResult, GitRequest, GitResult, HerdrRequest, HerdrResult,
    ReportFileRequest, ReportFileResult, SessionRequest, SessionResult,
};
use crate::engine::child::{SingleWorkerInput, SingleWorkerOutput};
use crate::engine::workflow::{StepDecision, Workflow, WorkflowContext, WorkflowEvent};
use crate::worker::prompt::playbook_check::PlaybookPin;
use crate::worker::prompt::watchdog::validate_report_content;

/// Pure configuration parameters for watchdog execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchdogConfig {
    /// Heartbeat: the longest the watchdog sleeps without re-observing.
    pub poll_sec: u64,
    pub stall_sec: u64,
    pub idle_sec: u64,
    pub idle_fallback_sec: u64,
    pub gone_sec: u64,
    pub nudge_pct: u64,
    pub hard_cap_sec: u64,
    pub is_fallback_idle: bool,
    pub expect_verdict: bool,
    /// `runs/.<name>.playbook` for this prompt. `None` when the worker was
    /// given no playbook; reviewer configs never carry one.
    #[serde(default)]
    pub playbook: Option<PlaybookPin>,
    #[serde(default)]
    pub land_sec: u64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            poll_sec: 60,
            stall_sec: 600,
            idle_sec: 120,
            idle_fallback_sec: 300,
            gone_sec: 60,
            nudge_pct: 80,
            hard_cap_sec: 7200,
            is_fallback_idle: false,
            expect_verdict: false,
            playbook: None,
            land_sec: 0,
        }
    }
}

/// State of an active single worker workflow execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SingleWorkerState {
    pub input: SingleWorkerInput,
    pub report_path: PathBuf,
    pub config: WatchdogConfig,
    pub start_ms: u64,
    #[serde(default)]
    pub prev_elapsed_ms: u64,
    #[serde(default, alias = "budget_deadline_ms")]
    pub allowance_deadline_ms: u64,
    #[serde(default)]
    pub cap_deadline_ms: u64,
    #[serde(default)]
    pub last_progress_ms: u64,
    #[serde(default)]
    pub first_progress_seen: bool,
    #[serde(default)]
    pub current_diff_hash: String,
    #[serde(default)]
    pub last_autoext_diff_hash: String,
    #[serde(default)]
    pub agent_status: String,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub hook_active: bool,
    #[serde(default)]
    pub idle_since_ms: u64,
    /// When a screen-inferred `blocked` was first seen, 0 when none is pending.
    /// herdr's screen detection reads a non-modal choice banner as a dialog, so
    /// that reading is a hypothesis until it persists with no progress behind it.
    #[serde(default)]
    pub blocked_since_ms: u64,
    #[serde(default)]
    pub gone_since_ms: u64,
    #[serde(default, alias = "grace_used")]
    pub overrun_granted: bool,
    #[serde(default, alias = "autoext_total_ms")]
    pub extension_total_ms: u64,
    #[serde(default)]
    pub poll_count: u64,
    #[serde(default)]
    pub nudged: bool,
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub report_mtime_ms: u64,
    #[serde(default)]
    pub report_ready: bool,
    #[serde(default)]
    pub session_mtime_ms: u64,
    #[serde(default)]
    pub session_key: String,
    #[serde(default)]
    pub session_seen: bool,
    #[serde(default)]
    pub wait_err: String,
    #[serde(default)]
    pub lead_waiting: bool,
    #[serde(default)]
    pub errors: u64,
}

impl SingleWorkerState {
    pub fn new(input: SingleWorkerInput) -> Self {
        let report_path = PathBuf::from(format!("runs/{}.md", input.name));
        let expect_verdict = input.brief.contains("Pipeline phase: REVIEW");
        let is_fallback_idle = crate::harness::mentions_fallback_idle(&input.kind);

        let config = WatchdogConfig {
            expect_verdict,
            is_fallback_idle,
            ..WatchdogConfig::default()
        };

        Self {
            input,
            report_path,
            config,
            start_ms: 0,
            prev_elapsed_ms: 0,
            allowance_deadline_ms: 0,
            cap_deadline_ms: 0,
            last_progress_ms: 0,
            first_progress_seen: false,
            current_diff_hash: String::new(),
            last_autoext_diff_hash: String::new(),
            agent_status: "working".to_string(),
            harness: None,
            hook_active: true,
            idle_since_ms: 0,
            blocked_since_ms: 0,
            gone_since_ms: 0,
            overrun_granted: false,
            extension_total_ms: 0,
            poll_count: 0,
            nudged: false,
            initialized: false,
            report_mtime_ms: 0,
            report_ready: false,
            session_mtime_ms: 0,
            session_key: String::new(),
            session_seen: false,
            wait_err: String::new(),
            lead_waiting: false,
            errors: 0,
        }
    }

    pub fn with_config(mut self, config: WatchdogConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_report_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.report_path = path.into();
        self
    }

    pub fn resumed(
        mut self,
        prev_elapsed_sec: u64,
        overrun_granted: bool,
        extension_total_sec: u64,
        nudged: bool,
        last_progress_sec: u64,
    ) -> Self {
        self.prev_elapsed_ms = prev_elapsed_sec * 1000;
        self.overrun_granted = overrun_granted;
        self.extension_total_ms = extension_total_sec * 1000;
        self.nudged = nudged;
        if last_progress_sec > 0 {
            self.last_progress_ms = last_progress_sec * 1000;
            self.first_progress_seen = true;
        }
        self.initialized = false;
        self.start_ms = 0;
        self
    }

    fn idle_limit_ms(&self) -> u64 {
        let is_fallback_idle = (!self.hook_active)
            && self
                .harness
                .as_deref()
                .and_then(crate::harness::Harness::parse)
                .is_some_and(|h| h.fallback_idle())
            || self.config.is_fallback_idle;
        if is_fallback_idle {
            self.config.idle_fallback_sec
        } else {
            self.config.idle_sec
        }
    }

    fn has_progress_source(&self) -> bool {
        self.session_seen || !self.current_diff_hash.is_empty()
    }
}

/// Deterministic SingleWorkerWorkflow state machine.
#[derive(Debug, Clone, Default)]
pub struct SingleWorkerWorkflow;

impl SingleWorkerWorkflow {
    pub fn new() -> Self {
        Self
    }
}

fn complete_step(
    state: &mut SingleWorkerState,
    now_ms: u64,
    exit: i32,
) -> StepDecision<SingleWorkerOutput> {
    if state.start_ms > 0 && now_ms >= state.start_ms {
        let added = now_ms - state.start_ms;
        state.prev_elapsed_ms =
            (state.prev_elapsed_ms + added).min(state.input.time_budget_sec * 1000);
        state.start_ms = now_ms;
    }
    StepDecision::Complete(
        SingleWorkerOutput::new(exit, state.report_path.clone())
            .expect("valid single worker exit code"),
    )
}

/// One blocking wait, clamped to the next deadline. `until` empty means the
/// herdr default set (`idle`, `done`, `blocked`).
fn schedule_wake(
    state: &SingleWorkerState,
    ctx: &mut WorkflowContext,
    index: u64,
    until: Vec<String>,
    timeout_ms: u64,
) {
    ctx.schedule_activity(
        format!("wake_{index}"),
        ActivityRequest::Herdr(HerdrRequest::AgentWait {
            name: state.input.name.clone(),
            until,
            timeout_ms,
        }),
    );
}

impl Workflow for SingleWorkerWorkflow {
    type Input = SingleWorkerInput;
    type State = SingleWorkerState;
    type Output = SingleWorkerOutput;

    fn step(
        &self,
        state: &mut Self::State,
        ctx: &mut WorkflowContext,
        event: Option<WorkflowEvent>,
    ) -> StepDecision<Self::Output> {
        if !state.initialized {
            state.initialized = true;
            state.start_ms = ctx.now_ms();
            let rem_budget = state
                .input
                .time_budget_sec
                .saturating_sub(state.prev_elapsed_ms / 1000)
                .max(1);
            let rem_cap = state
                .config
                .hard_cap_sec
                .saturating_sub(state.prev_elapsed_ms / 1000)
                .max(1);
            state.allowance_deadline_ms = state.start_ms + rem_budget * 1000;
            state.cap_deadline_ms = state.start_ms + rem_cap * 1000;
            schedule_wake(state, ctx, 0, Vec::new(), 0);
            return StepDecision::Continue;
        }

        match event {
            None => StepDecision::Continue,

            Some(WorkflowEvent::ActivityCompleted { id, result }) => {
                if let Some(index) = parse_index(&id, "wake_") {
                    let now = ctx.now_ms();
                    let (
                        status,
                        session_kind,
                        session_value,
                        harness,
                        hook_active,
                        lead_waiting,
                        err,
                    ) = match result {
                        ActivityResult::Herdr(HerdrResult::AgentWait {
                            status,
                            session_kind,
                            session_value,
                            harness,
                            hook_active,
                            lead_waiting,
                            err,
                            ..
                        }) => (
                            status,
                            session_kind,
                            session_value,
                            harness,
                            hook_active,
                            lead_waiting,
                            err,
                        ),
                        _ => {
                            state.errors += 1;
                            state.gone_since_ms = 0;
                            state.wait_err = format!("wake {index}: unexpected activity result");
                            if now >= state.cap_deadline_ms {
                                return complete_step(state, now, 2);
                            }
                            if state.errors >= 3 {
                                return complete_step(state, now, 3);
                            }
                            let backoff = (30_000u64).min(state.config.poll_sec * 1000);
                            let t = wait_interval(state, now, now + backoff);
                            schedule_wake(state, ctx, index + 1, Vec::new(), t);
                            return StepDecision::Continue;
                        }
                    };

                    state.agent_status = status;
                    state.session_key = format!("{session_kind}:{session_value}");
                    state.harness = if harness.is_empty() {
                        None
                    } else {
                        Some(harness)
                    };
                    state.hook_active = hook_active;
                    state.lead_waiting = lead_waiting;
                    state.wait_err = err.clone();

                    if err == "agent_not_running" || err == "agent_not_found" {
                        if state.gone_since_ms == 0 {
                            state.gone_since_ms = now;
                        }
                        if now >= state.cap_deadline_ms {
                            return complete_step(state, now, 2);
                        }
                        let gone_end = state.gone_since_ms + state.config.gone_sec * 1000;
                        if now >= gone_end {
                            return complete_step(state, now, 3);
                        }
                        let t = wait_interval(state, now, gone_end);
                        schedule_wake(state, ctx, index + 1, Vec::new(), t);
                        return StepDecision::Continue;
                    }

                    if !err.is_empty() {
                        // Socket/io error: back off and re-observe; never hot-loop.
                        state.errors += 1;
                        state.gone_since_ms = 0;
                        if now >= state.cap_deadline_ms {
                            return complete_step(state, now, 2);
                        }
                        let backoff = (30_000u64).min(state.config.poll_sec * 1000);
                        let t = wait_interval(state, now, now + backoff);
                        schedule_wake(state, ctx, index + 1, Vec::new(), t);
                        return StepDecision::Continue;
                    }

                    state.errors = 0;
                    state.gone_since_ms = 0;
                    state.poll_count = index;
                    ctx.schedule_activity(
                        format!("obs_{index}_report"),
                        ActivityRequest::ReportFile(ReportFileRequest::CheckMtime {
                            path: state.report_path.to_string_lossy().to_string(),
                        }),
                    );
                    ctx.schedule_activity(
                        format!("obs_{index}_diff"),
                        ActivityRequest::Git(GitRequest::DiffHash {
                            path: state.input.repo.clone(),
                        }),
                    );
                    ctx.schedule_activity(
                        format!("obs_{index}_session"),
                        ActivityRequest::Session(SessionRequest::Mtime {
                            harness: state.harness.clone().unwrap_or_default(),
                            session_kind: state
                                .session_key
                                .split_once(':')
                                .map(|(k, _)| k.to_string())
                                .unwrap_or_default(),
                            session_value: state
                                .session_key
                                .split_once(':')
                                .map(|(_, v)| v.to_string())
                                .unwrap_or_default(),
                            cwd: String::new(),
                        }),
                    );
                    return StepDecision::Continue;
                }

                if let Some(index) = parse_index(&id, "obs_") {
                    let now = ctx.now_ms();
                    if id.ends_with("_report") {
                        if let ActivityResult::ReportFile(ReportFileResult::MtimeMs(mtime)) = result
                        {
                            state.report_mtime_ms = mtime;
                            state.report_ready = mtime > 0;
                        }
                        return StepDecision::Continue;
                    }
                    if id.ends_with("_diff") {
                        if let ActivityResult::Git(GitResult::DiffHash { hash, .. }) = result {
                            if !state.first_progress_seen {
                                state.first_progress_seen = true;
                                state.current_diff_hash = hash.clone();
                                if state.last_autoext_diff_hash.is_empty() && !hash.is_empty() {
                                    state.last_autoext_diff_hash = hash;
                                }
                            } else if hash != state.current_diff_hash {
                                state.current_diff_hash = hash;
                                state.last_progress_ms = now;
                            }
                        }
                        return StepDecision::Continue;
                    }
                    if id.ends_with("_session") {
                        if let ActivityResult::Session(SessionResult::Mtime(mtime)) = result {
                            match mtime {
                                Some(ms) => {
                                    if !state.session_seen {
                                        state.session_seen = true;
                                        state.session_mtime_ms = ms;
                                    } else if ms > state.session_mtime_ms {
                                        state.session_mtime_ms = ms;
                                        state.last_progress_ms = now;
                                    }
                                }
                                None => state.session_seen = false,
                            }
                        }
                        return evaluate(state, ctx, index, now);
                    }
                    return StepDecision::Continue;
                }

                if let Some(index) = parse_index(&id, "read_report_") {
                    if let ActivityResult::ReportFile(ReportFileResult::Content(content)) = result {
                        if content.trim().is_empty() {
                            state.report_ready = false;
                            let t = wait_interval(
                                state,
                                ctx.now_ms(),
                                ctx.now_ms() + state.config.poll_sec * 1000,
                            );
                            schedule_wake(state, ctx, index + 1, default_until(state), t);
                            return StepDecision::Continue;
                        }
                        let malformed =
                            validate_report_content(&content, state.config.expect_verdict).is_err();
                        // § S8: a builder report must account for every step
                        // pinned for this prompt. Reviewer reports carry a
                        // `Verdict:` line instead and never carry a pin.
                        let playbook_gap = !state.config.expect_verdict
                            && state
                                .config
                                .playbook
                                .as_ref()
                                .is_some_and(|pin| !pin.gap(&content).is_empty());
                        let exit = if malformed || playbook_gap { 6 } else { 0 };
                        return complete_step(state, ctx.now_ms(), exit);
                    }
                    return StepDecision::Continue;
                }

                StepDecision::Continue
            }

            Some(WorkflowEvent::TimerFired { id }) => {
                if let Some(index) = parse_index(&id, "backoff_") {
                    schedule_wake(state, ctx, index + 1, Vec::new(), 0);
                }
                StepDecision::Continue
            }

            Some(unexpected) => StepDecision::Fail {
                code: 1,
                reason: format!("unexpected workflow event: {unexpected:?}"),
            },
        }
    }
}

fn parse_index(id: &str, prefix: &str) -> Option<u64> {
    id.strip_prefix(prefix)?
        .split('_')
        .next()?
        .parse::<u64>()
        .ok()
}

fn default_until(state: &SingleWorkerState) -> Vec<String> {
    if state.agent_status == "blocked" {
        if state.blocked_since_ms > 0 {
            // An unconfirmed screen-inferred blocked: wait for the pane to
            // leave it. The default set contains `blocked`, so it would return
            // at once and decide on the same frame.
            vec![
                "working".to_string(),
                "idle".to_string(),
                "done".to_string(),
            ]
        } else {
            Vec::new()
        }
    } else if state.agent_status == "idle" || state.agent_status == "done" {
        // The current state already matches the default set; flip so the wait
        // returns only when the agent starts working again (U1a spin).
        vec!["working".to_string(), "blocked".to_string()]
    } else if state.agent_status == "working" {
        // Sleep inside herdr agent wait matching blocked or done while working (P6).
        vec!["blocked".to_string(), "done".to_string()]
    } else {
        Vec::new()
    }
}

fn wait_interval(state: &SingleWorkerState, now: u64, next_deadline: u64) -> u64 {
    let heartbeat = now + state.config.poll_sec * 1000;
    next_deadline.min(heartbeat).saturating_sub(now).max(1000)
}

fn nudge_deadline_ms(state: &SingleWorkerState) -> u64 {
    let nudge_thresh_ms = (state.input.time_budget_sec * state.config.nudge_pct / 100) * 1000;
    state
        .start_ms
        .saturating_add(nudge_thresh_ms)
        .saturating_sub(state.prev_elapsed_ms)
}

/// Evaluate one wake after its report/diff/session observations completed.
fn evaluate(
    state: &mut SingleWorkerState,
    ctx: &mut WorkflowContext,
    index: u64,
    now: u64,
) -> StepDecision<SingleWorkerOutput> {
    // 1. Report first: one ready check per wake, then read.
    if state.report_ready {
        ctx.schedule_activity(
            format!("read_report_{index}"),
            ActivityRequest::ReportFile(ReportFileRequest::Read {
                path: state.report_path.to_string_lossy().to_string(),
            }),
        );
        return StepDecision::Continue;
    }

    // 2. Hard cap.
    if now >= state.cap_deadline_ms {
        return complete_step(state, now, 2);
    }

    // 3. Blocked on a question/permission.
    if state.agent_status == "blocked" {
        // A hook-reported blocked is the agent's own word: end the wait now.
        // A screen-inferred one is a single pane frame, and a non-modal choice
        // banner ("⚠ Warning: change effort … compact first? [Compact][Skip]")
        // has the footer shape of a dialog while the turn keeps running. When
        // there is a progress source to check it against, that reading is a
        // hypothesis until it persists for a poll with no progress behind it.
        if state.hook_active || !state.has_progress_source() {
            return complete_step(state, now, 5);
        }
        let progressed = state.last_progress_ms > state.blocked_since_ms;
        if state.blocked_since_ms == 0 || progressed {
            state.blocked_since_ms = now;
        } else if now.saturating_sub(state.blocked_since_ms) >= state.config.poll_sec * 1000 {
            return complete_step(state, now, 5);
        }
    } else {
        state.blocked_since_ms = 0;
    }

    // 4. Budget, with one grace and bounded diff auto-extensions.
    if now >= state.allowance_deadline_ms {
        if !state.overrun_granted
            && state.last_progress_ms > 0
            && now.saturating_sub(state.last_progress_ms) <= state.config.poll_sec * 2 * 1000
        {
            let mut grace_ms = (state.input.time_budget_sec / 4).max(1) * 1000;
            if state.allowance_deadline_ms + grace_ms > state.cap_deadline_ms {
                grace_ms = state
                    .cap_deadline_ms
                    .saturating_sub(state.allowance_deadline_ms);
            }
            if grace_ms > 0 {
                state.allowance_deadline_ms += grace_ms;
                state.overrun_granted = true;
            }
        }

        let max_autoext_ms = (state.input.time_budget_sec / 2) * 1000;
        if state.extension_total_ms < max_autoext_ms
            && !state.current_diff_hash.is_empty()
            && state.current_diff_hash != state.last_autoext_diff_hash
        {
            let rem = max_autoext_ms.saturating_sub(state.extension_total_ms);
            let mut ext_ms = ((state.input.time_budget_sec / 4).max(1) * 1000).min(rem);
            if state.allowance_deadline_ms + ext_ms > state.cap_deadline_ms {
                ext_ms = state
                    .cap_deadline_ms
                    .saturating_sub(state.allowance_deadline_ms);
            }
            if ext_ms > 0 {
                state.allowance_deadline_ms += ext_ms;
                state.extension_total_ms += ext_ms;
                state.last_autoext_diff_hash = state.current_diff_hash.clone();
            }
        }

        if now >= state.allowance_deadline_ms {
            return complete_step(state, now, 4);
        }
    }

    // 5. Idle without a report.
    if state.agent_status == "idle" || state.agent_status == "done" {
        if state.lead_waiting {
            state.idle_since_ms = 0;
        } else {
            if state.idle_since_ms == 0 {
                state.idle_since_ms = now;
            }
            if now.saturating_sub(state.idle_since_ms) >= state.idle_limit_ms() * 1000 {
                return complete_step(state, now, 3);
            }
        }
    } else {
        state.idle_since_ms = 0;
    }

    // 6. Stall: only when a progress source exists and the agent is working.
    if state.agent_status == "working" && !state.lead_waiting && state.has_progress_source() {
        let baseline = if state.last_progress_ms > 0 {
            state.last_progress_ms
        } else {
            state.start_ms
        };
        if now.saturating_sub(baseline) >= state.config.stall_sec * 1000 {
            return complete_step(state, now, 3);
        }
    }

    // 7. Nudge once at nudge_pct.
    let nudge_at_ms = nudge_deadline_ms(state);
    if !state.nudged && now >= nudge_at_ms {
        state.nudged = true;
        let nudge_msg = format!(
            "Time check: you are past {}% of this task's time budget. Wrap up now: finish the current step, commit your work, and write {} (via a temp file + rename) with what is done plus a Blockers section for the rest. Reply only the path.",
            state.config.nudge_pct,
            state.report_path.display()
        );
        ctx.schedule_activity(
            format!("nudge_{index}"),
            ActivityRequest::Herdr(HerdrRequest::Prompt {
                name: state.input.name.clone(),
                task: nudge_msg,
            }),
        );
    }

    // 8. Next wake, clamped to the earliest deadline.
    let mut deadline = now + state.config.poll_sec * 1000;
    deadline = deadline.min(state.cap_deadline_ms);
    deadline = deadline.min(state.allowance_deadline_ms);
    if !state.nudged {
        deadline = deadline.min(nudge_at_ms);
    }
    if state.agent_status == "idle" || state.agent_status == "done" {
        if !state.lead_waiting {
            deadline = deadline.min(state.idle_since_ms + state.idle_limit_ms() * 1000);
        }
    } else if state.agent_status == "blocked" && state.blocked_since_ms > 0 {
        deadline = deadline.min(state.blocked_since_ms + state.config.poll_sec * 1000);
    } else if state.agent_status == "working" && !state.lead_waiting && state.has_progress_source()
    {
        let baseline = if state.last_progress_ms > 0 {
            state.last_progress_ms
        } else {
            state.start_ms
        };
        deadline = deadline.min(baseline + state.config.stall_sec * 1000);
    }
    let timeout_ms = deadline.saturating_sub(now).max(1000);
    schedule_wake(state, ctx, index + 1, default_until(state), timeout_ms);
    StepDecision::Continue
}

// ----------------------------------------------------------------------------
// Deterministic Testkit Test Suite
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testkit::{
        run_to_completion, InMemoryJournal, MockActivities, VirtualClock,
    };

    fn sample_input(name: &str, budget_sec: u64) -> SingleWorkerInput {
        SingleWorkerInput::new(
            name,
            "sude",
            "quaqua",
            "Goal: implement worker workflow\nHard constraints: none\nEvidence: tests pass",
            budget_sec,
            None,
        )
    }

    #[derive(Clone)]
    struct Poll {
        status: &'static str,
        err: &'static str,
        diff: &'static str,
        report_mtime: u64,
        session_mtime: Option<u64>,
        report: Option<&'static str>,
        hook_active: bool,
    }

    impl Default for Poll {
        fn default() -> Self {
            Self {
                status: "working",
                err: "",
                diff: "dh0",
                report_mtime: 0,
                session_mtime: Some(1000),
                report: None,
                hook_active: true,
            }
        }
    }

    fn register_poll(activities: &mut MockActivities, name: &str, poll: u64, p: Poll) {
        activities.register(
            format!("wake_{poll}"),
            ActivityResult::Herdr(HerdrResult::AgentWait {
                name: name.to_string(),
                status: p.status.to_string(),
                seq: poll + 1,
                session_kind: "id".to_string(),
                session_value: "sid".to_string(),
                harness: "omp".to_string(),
                cwd: "/repo".to_string(),
                err: p.err.to_string(),
                lead_waiting: false,
                hook_active: p.hook_active,
            }),
        );
        activities.register(
            format!("obs_{poll}_report"),
            ActivityResult::ReportFile(ReportFileResult::MtimeMs(p.report_mtime)),
        );
        activities.register(
            format!("obs_{poll}_diff"),
            ActivityResult::Git(GitResult::DiffHash {
                hash: p.diff.to_string(),
                stat: String::new(),
            }),
        );
        activities.register(
            format!("obs_{poll}_session"),
            ActivityResult::Session(SessionResult::Mtime(p.session_mtime)),
        );
        activities.register(
            format!("nudge_{poll}"),
            ActivityResult::Herdr(HerdrResult::Prompted {
                name: name.to_string(),
            }),
        );
        if let Some(report) = p.report {
            activities.register(
                format!("read_report_{poll}"),
                ActivityResult::ReportFile(ReportFileResult::Content(report.to_string())),
            );
        }
    }

    fn config(heartbeat: u64, budget_cap: u64) -> WatchdogConfig {
        WatchdogConfig {
            poll_sec: heartbeat,
            hard_cap_sec: budget_cap,
            stall_sec: 600,
            nudge_pct: 100,
            ..WatchdogConfig::default()
        }
    }

    fn run(
        name: &str,
        budget: u64,
        cfg: WatchdogConfig,
        polls: Vec<Poll>,
    ) -> (
        SingleWorkerOutput,
        u64,
        Vec<crate::engine::journal::EventRecord>,
    ) {
        let workflow = SingleWorkerWorkflow::new();
        let mut clock = VirtualClock::new(100_000);
        let mut activities = MockActivities::new();
        let mut journal = InMemoryJournal::new();
        for (i, p) in polls.iter().cloned().enumerate() {
            register_poll(&mut activities, name, i as u64, p);
        }
        let state = SingleWorkerState::new(sample_input(name, budget)).with_config(cfg);
        let out = run_to_completion(
            &workflow,
            &format!("test-{name}"),
            state,
            &mut clock,
            &mut activities,
            &mut journal,
        )
        .expect("workflow completes");
        let now = clock.now_ms();
        (
            out,
            now,
            journal.all_events(&format!("test-{name}")).to_vec(),
        )
    }

    #[test]
    fn exit_0_on_first_wake_with_a_ready_report() {
        let (out, now, _) = run(
            "worker-ok",
            1800,
            config(30, 200),
            vec![Poll {
                report_mtime: 1000,
                report: Some("Outcome: COMPLETE\n# Report\nDone!\n\n## Lessons\n- none\n"),
                ..Poll::default()
            }],
        );
        assert_eq!(out.exit, 0);
        assert_eq!(now, 100_000, "one wake, no second poll");
    }

    #[test]
    fn exit_6_on_stable_malformed_report() {
        let (out, _, _) = run(
            "worker-bad",
            1800,
            config(30, 200),
            vec![Poll {
                report_mtime: 1000,
                report: Some("# Bad Report\nMissing lessons\n"),
                ..Poll::default()
            }],
        );
        assert_eq!(out.exit, 6);
    }

    #[test]
    fn exit_6_when_a_pinned_playbook_step_is_not_accounted_for() {
        let mut cfg = config(30, 200);
        cfg.playbook = Some(PlaybookPin {
            name: "feature".into(),
            ids: vec![1, 2],
        });
        let (out, _, _) = run(
            "worker-gap",
            1800,
            cfg,
            vec![Poll {
                report_mtime: 1000,
                report: Some("Outcome: COMPLETE\n# Report\n\n## Lessons\nnone\n"),
                ..Poll::default()
            }],
        );
        assert_eq!(
            out.exit, 6,
            "a missing `## Playbook` block rejects the report"
        );
    }

    #[test]
    fn exit_0_when_every_pinned_playbook_step_is_accounted_for() {
        let mut cfg = config(30, 200);
        cfg.playbook = Some(PlaybookPin {
            name: "feature".into(),
            ids: vec![1, 2],
        });
        let (out, _, _) = run(
            "worker-pinned",
            1800,
            cfg,
            vec![Poll {
                report_mtime: 1000,
                report: Some(
                    "Outcome: COMPLETE\n# Report\n\n## Playbook\n1: done\n2: skip: unsupported on omp\n\n## Lessons\nnone\n",
                ),
                ..Poll::default()
            }],
        );
        assert_eq!(out.exit, 0);
    }

    #[test]
    fn exit_0_for_a_reviewer_report_even_with_a_pin() {
        let mut cfg = config(30, 200);
        cfg.expect_verdict = true;
        cfg.playbook = Some(PlaybookPin {
            name: "feature".into(),
            ids: vec![1, 2],
        });
        let (out, _, _) = run(
            "worker-review",
            1800,
            cfg,
            vec![Poll {
                report_mtime: 1000,
                report: Some("# Review\n\nVerdict: clean\n\n## Lessons\nnone\n"),
                ..Poll::default()
            }],
        );
        assert_eq!(out.exit, 0, "reviewer reports are never checked");
    }

    #[test]
    fn exit_2_at_the_hard_cap_with_a_moving_diff() {
        let polls = (0..4)
            .map(|i| Poll {
                diff: ["dh0", "dh1", "dh2", "dh3"][i],
                session_mtime: None,
                ..Poll::default()
            })
            .collect();
        let (out, now, _) = run("worker-cap", 10_000, config(30, 90), polls);
        assert_eq!(out.exit, 2);
        assert_eq!(now, 100_000 + 90_000);
    }

    #[test]
    fn exit_3_stall_when_neither_diff_nor_session_moves() {
        let polls = (0..3)
            .map(|_| Poll {
                session_mtime: Some(1000),
                ..Poll::default()
            })
            .collect();
        let mut cfg = config(30, 1800);
        cfg.stall_sec = 60;
        let (out, now, _) = run("worker-stall", 1800, cfg, polls);
        assert_eq!(out.exit, 3);
        assert_eq!(now, 100_000 + 60_000);
    }

    #[test]
    fn stall_is_skipped_when_the_harness_has_no_progress_source() {
        let polls = (0..5)
            .map(|_| Poll {
                diff: "",
                session_mtime: None,
                ..Poll::default()
            })
            .collect();
        let mut cfg = config(30, 1800);
        cfg.stall_sec = 60;
        let (out, _, _) = run("worker-blind", 100, cfg, polls);
        // No source: never a stall, the budget alone ends it.
        assert_eq!(out.exit, 4);
    }

    #[test]
    fn exit_3_idle_after_idle_sec() {
        let polls = (0..3)
            .map(|_| Poll {
                status: "idle",
                ..Poll::default()
            })
            .collect();
        let mut cfg = config(30, 1800);
        cfg.idle_sec = 60;
        let (out, now, _) = run("worker-idle", 1800, cfg, polls);
        assert_eq!(out.exit, 3);
        assert_eq!(now, 100_000 + 60_000);
    }

    #[test]
    fn idle_fallback_uses_300s_for_opencode_without_a_hook() {
        let polls = (0..11)
            .map(|_| Poll {
                status: "idle",
                hook_active: false,
                ..Poll::default()
            })
            .collect();
        let mut cfg = config(30, 1800);
        cfg.idle_sec = 60;
        cfg.idle_fallback_sec = 300;
        let (out, now, _) = run("worker-fb", 1800, cfg, polls);
        assert_eq!(out.exit, 3);
        assert_eq!(now, 100_000 + 300_000);
    }

    #[test]
    fn exit_3_agent_gone_after_gone_sec() {
        let polls = (0..3)
            .map(|_| Poll {
                status: "gone",
                err: "agent_not_found",
                ..Poll::default()
            })
            .collect();
        let (out, now, _) = run("worker-gone", 1800, config(30, 1800), polls);
        assert_eq!(out.exit, 3);
        assert_eq!(now, 100_000 + 60_000);
    }

    #[test]
    fn exit_4_at_the_budget_without_progress() {
        let polls = (0..4)
            .map(|_| Poll {
                session_mtime: Some(1000),
                ..Poll::default()
            })
            .collect();
        let mut cfg = config(30, 1800);
        cfg.stall_sec = 9999;
        let (out, now, _) = run("worker-budget", 60, cfg, polls);
        assert_eq!(out.exit, 4);
        assert_eq!(now, 100_000 + 60_000);
    }

    #[test]
    fn exit_5_on_blocked_status() {
        let (out, _, _) = run(
            "worker-blocked",
            1800,
            config(30, 1800),
            vec![Poll {
                status: "blocked",
                ..Poll::default()
            }],
        );
        assert_eq!(out.exit, 5);
    }

    /// herdr's screen detection reads a non-modal choice banner
    /// (`⚠ Warning: change effort … compact first? [Compact][Skip]`) as a
    /// pending question while the turn keeps running: the agent's session store
    /// advances under the banner. That is not a blocked worker, and the wait
    /// must not end at exit 5 on it.
    #[test]
    fn screen_inferred_blocked_banner_does_not_end_the_wait_while_the_agent_progresses() {
        let polls = vec![
            Poll {
                status: "blocked",
                hook_active: false,
                session_mtime: Some(1000),
                ..Poll::default()
            },
            Poll {
                status: "blocked",
                hook_active: false,
                session_mtime: Some(2000),
                ..Poll::default()
            },
            Poll {
                status: "working",
                hook_active: false,
                session_mtime: Some(3000),
                ..Poll::default()
            },
            Poll {
                status: "done",
                hook_active: false,
                session_mtime: Some(4000),
                report_mtime: 1000,
                report: Some("Outcome: COMPLETE\n# Report\n\n## Lessons\n- none\n"),
                ..Poll::default()
            },
        ];
        let (out, _, _) = run("worker-banner", 1800, config(30, 1800), polls);
        assert_eq!(
            out.exit, 0,
            "a banner over a progressing turn must not end the wait as blocked"
        );
    }

    /// The same screen reading with no progress behind it is a real question:
    /// it persists and the wait still ends at exit 5.
    #[test]
    fn screen_inferred_blocked_with_no_progress_still_ends_the_wait_as_blocked() {
        let polls = vec![
            Poll {
                status: "blocked",
                hook_active: false,
                session_mtime: Some(1000),
                ..Poll::default()
            },
            Poll {
                status: "blocked",
                hook_active: false,
                session_mtime: Some(1000),
                ..Poll::default()
            },
        ];
        let (out, _, _) = run("worker-question", 1800, config(30, 1800), polls);
        assert_eq!(
            out.exit, 5,
            "a question that persists with no progress must end the wait as blocked"
        );
    }

    #[test]
    fn exit_5_latency_under_5s_on_working_to_blocked_transition() {
        let (out, now, events) = run(
            "worker-working-to-blocked",
            1800,
            config(30, 1800),
            vec![
                Poll {
                    status: "working",
                    ..Poll::default()
                },
                Poll {
                    status: "blocked",
                    ..Poll::default()
                },
            ],
        );
        assert_eq!(out.exit, 5, "exit code must be 5 (blocked)");
        let latency_ms = now.saturating_sub(100_000);
        assert!(
            latency_ms < 5000,
            "exit 5 latency must be under 5s on working->blocked transition, was {latency_ms}ms"
        );
        let wake_1_sched = events
            .iter()
            .find(|e| {
                e.kind == "activity_scheduled"
                    && e.payload_json.contains("AgentWait")
                    && e.payload_json.contains("\"timeout_ms\":30000")
            })
            .expect("wake_1 AgentWait must be scheduled");
        assert!(
            wake_1_sched
                .payload_json
                .contains("\"until\":[\"blocked\",\"done\"]"),
            "wake_1 while working must specify --until blocked --until done, got: {}",
            wake_1_sched.payload_json
        );
    }

    #[test]
    fn grace_extends_a_progressing_budget() {
        let polls = vec![
            Poll::default(),
            Poll::default(),
            Poll {
                diff: "dh1",
                ..Poll::default()
            },
            Poll {
                diff: "dh1",
                report_mtime: 1000,
                report: Some(
                    "Outcome: COMPLETE\n# Report\nLanded in grace\n\n## Lessons\n- none\n",
                ),
                ..Poll::default()
            },
        ];
        let (out, now, _) = run("worker-grace", 40, config(20, 200), polls);
        assert_eq!(out.exit, 0);
        assert_eq!(now, 100_000 + 60_000);
    }

    #[test]
    fn autoext_is_bounded_by_half_the_budget() {
        let polls = (0..5)
            .map(|i| Poll {
                diff: ["dh0", "dh1", "dh2", "dh3", "dh4"][i],
                ..Poll::default()
            })
            .collect();
        let (out, _, _) = run("worker-ext", 40, config(20, 200), polls);
        assert_eq!(out.exit, 4);
    }

    #[test]
    fn nudge_is_sent_once_at_the_threshold() {
        let polls = vec![
            Poll::default(),
            Poll::default(),
            Poll::default(),
            Poll::default(),
            Poll::default(),
            Poll {
                report_mtime: 1000,
                report: Some("Outcome: COMPLETE\n# Report\nNudged\n\n## Lessons\n- none\n"),
                ..Poll::default()
            },
        ];
        let mut cfg = config(20, 400);
        cfg.nudge_pct = 80;
        let (out, _, events) = run("worker-nudge", 100, cfg, polls);
        assert_eq!(out.exit, 0);
        assert!(events
            .iter()
            .any(|e| e.payload_json.contains("Time check: you are past 80%")));
    }

    #[test]
    fn a_socket_error_backs_off_without_observations() {
        let polls = vec![
            Poll::default(),
            Poll {
                err: "socket is closed",
                ..Poll::default()
            },
            Poll {
                report_mtime: 1000,
                report: Some("Outcome: COMPLETE\n# Report\nRecovered\n\n## Lessons\n- none\n"),
                ..Poll::default()
            },
        ];
        let (out, _, _) = run("worker-err", 1800, config(20, 1800), polls);
        assert_eq!(out.exit, 0);
    }

    #[test]
    fn deterministic_two_runs_produce_identical_events() {
        let polls = vec![
            Poll::default(),
            Poll {
                diff: "dh1",
                ..Poll::default()
            },
            Poll {
                diff: "dh1",
                report_mtime: 1000,
                report: Some("Outcome: COMPLETE\n# Report\nDeterministic\n\n## Lessons\n- none\n"),
                ..Poll::default()
            },
        ];
        let (out1, now1, events1) = run("worker-det", 1800, config(20, 1800), polls.clone());
        let (out2, now2, events2) = run("worker-det", 1800, config(20, 1800), polls);
        assert_eq!(out1, out2);
        assert_eq!(now1, now2);
        assert_eq!(events1, events2);
    }

    #[test]
    fn unexpected_wake_activity_result_retries_and_recovers() {
        let workflow = SingleWorkerWorkflow::new();
        let mut clock = VirtualClock::new(100_000);
        let mut activities = MockActivities::new();
        let mut journal = InMemoryJournal::new();

        // wake_0 returns unexpected activity result (e.g. ActivityResult::Unknown)
        activities.register("wake_0", ActivityResult::Unknown);
        // wake_1 returns normal poll with report
        register_poll(
            &mut activities,
            "worker-rec",
            1,
            Poll {
                report_mtime: 1000,
                report: Some("Outcome: COMPLETE\n# Report\nRecovered\n\n## Lessons\n- none\n"),
                ..Poll::default()
            },
        );

        let state =
            SingleWorkerState::new(sample_input("worker-rec", 1800)).with_config(config(20, 1800));
        let outcome = run_to_completion(
            &workflow,
            "test-worker-rec",
            state,
            &mut clock,
            &mut activities,
            &mut journal,
        );
        let out = outcome.expect("workflow must recover via retry rather than failing");
        assert_eq!(out.exit, 0);
    }

    #[test]
    fn unexpected_wake_activity_result_exhausts_retries_to_exit_3() {
        let workflow = SingleWorkerWorkflow::new();
        let mut clock = VirtualClock::new(100_000);
        let mut activities = MockActivities::new();
        let mut journal = InMemoryJournal::new();

        activities.register("wake_0", ActivityResult::Unknown);
        activities.register("wake_1", ActivityResult::Unknown);
        activities.register("wake_2", ActivityResult::Unknown);
        activities.register("wake_3", ActivityResult::Unknown);

        let state =
            SingleWorkerState::new(sample_input("worker-fail", 1800)).with_config(config(20, 1800));
        let outcome = run_to_completion(
            &workflow,
            "test-worker-fail",
            state,
            &mut clock,
            &mut activities,
            &mut journal,
        );
        let out = outcome.expect("workflow must complete with exit 3 when wake retries exhausted");
        assert_eq!(out.exit, 3);
    }
}
