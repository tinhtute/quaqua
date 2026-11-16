//! The per-task flow (ported from the old `PipelineWorkflow` state machine):
//! build -> review on another harness/kind -> gate -> merge into the project
//! integration branch.
//!
//! What the port drops: the old machine ran each phase as a child workflow
//! and a second `PipelineState` key=value file mirrored it for the driver.
//! Here one workflow owns all state (the journal is the only home), the
//! phases are plain activities and Signals, and git facts enter as activity
//! results, so replay reads every input from the journal and never from git.
//!
//! R22: the candidate sha is the branch tip read through git after the
//! builder reports it (a claim is never trusted), the review is bound to that
//! sha, and the gate re-reads the tip: when it moved after review the
//! acceptance is refused and the flow pauses for the Lead.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::activity::{
    GitRequest, GitResult, HerdrRequest, HerdrResult, ReportFileRequest, ReportFileResult,
};
use crate::engine::{
    ActivityRequest, ActivityResult, StepDecision, Workflow, WorkflowContext, WorkflowEvent,
};

pub const KIND: &str = "pipeline";

pub const TIMEOUT: i32 = 10;
pub const PROMPT_FAILED: i32 = 12;
pub const REJECTED: i32 = 1;

/// Pause codes (the `blocked <code>:` of the pause reason).
pub const BLOCK_UNFINISHED: i32 = 5;
pub const BLOCK_BAD_REVIEW: i32 = 6;
pub const BLOCK_BAD_SHA: i32 = 7;
pub const BLOCK_SHA_MOVED: i32 = 8;
pub const BLOCK_NOT_VERIFIED: i32 = 13;
pub const BLOCK_MERGE_REFUSED: i32 = 4;

const DEFAULT_MAX_ROUNDS: u32 = 2;
const DEFAULT_TIMEOUT_MS: u64 = 4 * 60 * 60 * 1000;

fn default_max_rounds() -> u32 {
    DEFAULT_MAX_ROUNDS
}
fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineInput {
    pub task: String,
    pub builder_kind: String,
    pub reviewer_kind: String,
    pub brief: String,
    /// The repo the builder branch and the integration branch live in.
    pub repo: String,
    /// The builder's branch (the candidate lives on it).
    pub branch: String,
    pub integ: String,
    /// Absolute directory the workers publish their reports into.
    pub runs_dir: String,
    /// The herdr pane the reviewer is spawned into.
    #[serde(default)]
    pub pane: String,
    #[serde(default)]
    pub project_id: Option<i64>,
    /// The check the merge queue runs after the merge; none merges unchecked.
    #[serde(default)]
    pub check: Option<String>,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

impl PipelineInput {
    pub fn builder(&self) -> String {
        format!("{}-b", self.task)
    }
    pub fn reviewer(&self) -> String {
        format!("{}-r", self.task)
    }
    fn report(&self, file: String) -> String {
        format!("{}/{}", self.runs_dir.trim_end_matches('/'), file)
    }
    pub fn build_report(&self) -> String {
        self.report(format!("{}.build.md", self.task))
    }
    pub fn review_report(&self, round: u32) -> String {
        self.report(format!("{}.r{round}.md", self.task))
    }
    pub fn fix_report(&self, round: u32) -> String {
        self.report(format!("{}.fix{round}.md", self.task))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Armed,
    Build,
    SpawnReview,
    Review,
    Fix,
    Gate,
    Merge,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// Approval re-runs the stage that blocked.
    Restart,
    /// Approval grants one more round.
    RoundsExhausted,
    /// The tip moved after review; approval re-reviews the new tip.
    Moved,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Blocked {
    pub from: Stage,
    pub kind: BlockKind,
    pub code: i32,
    pub reason: String,
    pub moved_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineState {
    pub input: PipelineInput,
    pub stage: Stage,
    pub armed: bool,
    pub prompted: bool,
    pub report_seen: bool,
    pub reading: bool,
    pub round: u32,
    /// What the builder claimed in its report (never trusted on its own).
    pub claimed: Option<String>,
    /// The branch tip git returned after the claim: the candidate.
    pub candidate: Option<String>,
    /// The sha the current review is bound to.
    pub reviewed: Option<String>,
    pub findings: u32,
    pub review_changed: bool,
    pub blocked: Option<Blocked>,
}

impl PipelineState {
    pub fn new(input: PipelineInput) -> Self {
        PipelineState {
            input,
            stage: Stage::Armed,
            armed: false,
            prompted: false,
            report_seen: false,
            reading: false,
            round: 0,
            claimed: None,
            candidate: None,
            reviewed: None,
            findings: 0,
            review_changed: false,
            blocked: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineOutput {
    /// `merged`, `no_changes` or `disputed`.
    pub status: String,
    pub rounds: u32,
    pub candidate_sha: Option<String>,
    pub merge_sha: Option<String>,
}

type Decision = StepDecision<PipelineOutput>;

fn activity(id: &str, a: ActivityRequest) -> Decision {
    StepDecision::ScheduleActivity {
        id: id.to_string(),
        activity: a,
    }
}

fn done(status: &str, s: &PipelineState, merge_sha: Option<String>) -> Decision {
    StepDecision::Complete(PipelineOutput {
        status: status.to_string(),
        rounds: s.round,
        candidate_sha: s.reviewed.clone().or_else(|| s.candidate.clone()),
        merge_sha,
    })
}

fn block(s: &mut PipelineState, from: Stage, kind: BlockKind, code: i32, why: String) -> Decision {
    s.stage = Stage::Blocked;
    s.reading = false;
    let reason = format!("blocked {code}: {why}");
    s.blocked = Some(Blocked {
        from,
        kind,
        code,
        reason: why,
        moved_to: None,
    });
    StepDecision::PauseForApproval { reason }
}

fn enter_prompt(
    s: &mut PipelineState,
    stage: Stage,
    tag: &str,
    to: String,
    text: String,
) -> Decision {
    s.stage = stage;
    s.prompted = false;
    s.report_seen = false;
    s.reading = false;
    activity(
        &format!("prompt-{tag}"),
        ActivityRequest::Herdr(HerdrRequest::Prompt {
            name: to,
            task: text,
        }),
    )
}

fn enter_build(s: &mut PipelineState) -> Decision {
    let i = &s.input;
    let text = format!(
        "{}\n\nPipeline phase: BUILD. Commit on branch {} (repo {}). Write your report to {}. \
         End it with `Verified: <level> <full 40-hex sha of your last commit>`. A wrong premise is \
         disputed under '## Challenge' with evidence; a task already satisfied says 'Outcome: NO_CHANGES'.",
        i.brief,
        i.branch,
        i.repo,
        i.build_report()
    );
    let to = i.builder();
    enter_prompt(s, Stage::Build, "build", to, text)
}

fn enter_review(s: &mut PipelineState, round: u32) -> Decision {
    s.round = round;
    s.reviewed = s.candidate.clone();
    let sha = s.reviewed.clone().unwrap_or_default();
    let i = &s.input;
    let text = format!(
        "Pipeline phase: REVIEW (round {round}) of task '{}'.\nReview exactly commit {sha} on branch {} \
         (repo {}); do not edit code; run the checks the brief names in the foreground.\nBrief:\n{}\n\n\
         Write your report to {}. Tag each finding under '## Findings' as '- [critical|major|minor|trivial] <finding>'. \
         End with exactly one line: 'Verdict: clean' or 'Verdict: <N> findings'. Dispute a wrong premise under '## Challenge'.",
        i.task,
        i.branch,
        i.repo,
        i.brief,
        i.review_report(round)
    );
    let to = i.reviewer();
    enter_prompt(s, Stage::Review, &format!("review-{round}"), to, text)
}

fn enter_fix(s: &mut PipelineState) -> Decision {
    let round = s.round;
    let i = &s.input;
    let text = format!(
        "Pipeline phase: FIX (after review round {round}). Resolve the {} blocking findings in {} \
         on branch {}; commit each fix; a wrong finding is disputed under '## Challenge'. \
         Write your report to {} and end it with `Verified: <level> <full 40-hex sha of your last commit>`.",
        s.findings,
        i.review_report(round),
        i.branch,
        i.fix_report(round)
    );
    let to = i.builder();
    enter_prompt(s, Stage::Fix, &format!("fix-{round}"), to, text)
}

fn enter_gate(s: &mut PipelineState) -> Decision {
    s.stage = Stage::Gate;
    activity("gate-tip", ActivityRequest::Git(GitRequest::CurrentSha))
}

fn report_path(s: &PipelineState) -> String {
    match s.stage {
        Stage::Build => s.input.build_report(),
        Stage::Review => s.input.review_report(s.round),
        _ => s.input.fix_report(s.round),
    }
}

fn read_if_ready(s: &mut PipelineState) -> Decision {
    let waiting = matches!(s.stage, Stage::Build | Stage::Review | Stage::Fix);
    if waiting && s.prompted && s.report_seen && !s.reading {
        s.reading = true;
        let path = report_path(s);
        return activity(
            &format!("read-{:?}-{}", s.stage, s.round),
            ActivityRequest::ReportFile(ReportFileRequest::Read { path }),
        );
    }
    StepDecision::Continue
}

/// Re-enters the stage a blocked flow paused in, after the Lead approved.
fn resume(s: &mut PipelineState) -> Decision {
    let Some(b) = s.blocked.take() else {
        return StepDecision::Continue;
    };
    match b.kind {
        BlockKind::Restart => match b.from {
            Stage::Build => enter_build(s),
            Stage::Fix => enter_fix(s),
            Stage::Review => {
                let r = s.round.max(1);
                enter_review(s, r)
            }
            Stage::SpawnReview => spawn_reviewer(s),
            _ => enter_gate(s),
        },
        BlockKind::RoundsExhausted => {
            s.input.max_rounds = s.input.max_rounds.max(s.round + 1);
            enter_fix(s)
        }
        BlockKind::Moved => {
            s.candidate = b.moved_to.or_else(|| s.candidate.clone());
            let next = s.round + 1;
            s.input.max_rounds = s.input.max_rounds.max(next);
            enter_review(s, next)
        }
    }
}

fn spawn_reviewer(s: &mut PipelineState) -> Decision {
    s.stage = Stage::SpawnReview;
    activity(
        "spawn-review",
        ActivityRequest::Herdr(HerdrRequest::Spawn {
            name: s.input.reviewer(),
            kind: s.input.reviewer_kind.clone(),
            repo: s.input.repo.clone(),
            approve_kinds: Vec::new(),
        }),
    )
}

pub struct PipelineFlow;

impl PipelineFlow {
    fn on_report(&self, s: &mut PipelineState, content: &str) -> Decision {
        s.reading = false;
        if has_challenge(content) {
            return done("disputed", s, None);
        }
        let from = s.stage;
        if s.stage == Stage::Review {
            return match parse_verdict(content) {
                Err(why) => block(s, from, BlockKind::Restart, BLOCK_BAD_REVIEW, why),
                Ok(n) => {
                    s.findings = blocking_findings(&s.input.brief, content, n);
                    if s.findings == 0 {
                        return enter_gate(s);
                    }
                    s.review_changed = true;
                    if s.round >= s.input.max_rounds {
                        let why = format!(
                            "review rounds exhausted ({} rounds, {} blocking findings remain)",
                            s.round, s.findings
                        );
                        return block(s, from, BlockKind::RoundsExhausted, BLOCK_UNFINISHED, why);
                    }
                    enter_fix(s)
                }
            };
        }
        // Build or Fix report.
        if let Some(why) = unfinished(content) {
            return block(s, from, BlockKind::Restart, BLOCK_UNFINISHED, why);
        }
        if s.stage == Stage::Build && outcome_is_no_changes(content) {
            return done("no_changes", s, None);
        }
        match verified_sha(content) {
            Err((code, why)) => block(s, from, BlockKind::Restart, code, why),
            Ok(sha) => {
                s.claimed = Some(sha);
                activity(
                    &format!("bind-{}", s.round),
                    ActivityRequest::Git(GitRequest::CurrentSha),
                )
            }
        }
    }
}

impl Workflow for PipelineFlow {
    type Input = PipelineInput;
    type State = PipelineState;
    type Output = PipelineOutput;

    fn step(
        &self,
        s: &mut PipelineState,
        _ctx: &mut WorkflowContext,
        event: Option<WorkflowEvent>,
    ) -> Decision {
        match event {
            None if !s.armed => {
                s.armed = true;
                StepDecision::ScheduleTimer {
                    id: "deadline".into(),
                    duration: Duration::from_millis(s.input.timeout_ms),
                }
            }
            Some(WorkflowEvent::Signal { source, name, .. }) => {
                match (source.as_str(), name.as_str()) {
                    ("pipeline", "start") if s.stage == Stage::Armed => enter_build(s),
                    ("report", "appeared") => {
                        s.report_seen = true;
                        read_if_ready(s)
                    }
                    _ => StepDecision::Continue,
                }
            }
            Some(WorkflowEvent::ApprovalResolved { approved, reason }) => {
                if s.stage != Stage::Blocked {
                    return StepDecision::Continue;
                }
                if !approved {
                    return StepDecision::Fail {
                        code: REJECTED,
                        reason: reason.unwrap_or_else(|| "blocked gate aborted".into()),
                    };
                }
                resume(s)
            }
            Some(WorkflowEvent::TimerFired { id }) if id == "deadline" => {
                if s.stage == Stage::Blocked {
                    return StepDecision::Continue;
                }
                StepDecision::Fail {
                    code: TIMEOUT,
                    reason: format!("pipeline unfinished at the deadline (stage {:?})", s.stage),
                }
            }
            Some(WorkflowEvent::ActivityCompleted { id, result }) => {
                let kind = id.split('-').next().unwrap_or("");
                match (kind, result) {
                    ("prompt", ActivityResult::Herdr(HerdrResult::Prompted { .. })) => {
                        s.prompted = true;
                        read_if_ready(s)
                    }
                    ("prompt", _) => StepDecision::Fail {
                        code: PROMPT_FAILED,
                        reason: "herdr did not take the prompt".into(),
                    },
                    ("spawn", ActivityResult::Herdr(HerdrResult::Spawned { .. })) => {
                        enter_review(s, 1)
                    }
                    ("spawn", _) => block(
                        s,
                        Stage::SpawnReview,
                        BlockKind::Restart,
                        BLOCK_UNFINISHED,
                        "the reviewer could not be spawned".into(),
                    ),
                    ("read", ActivityResult::ReportFile(ReportFileResult::Content(text))) => {
                        self.on_report(s, &text)
                    }
                    ("read", _) => {
                        let from = s.stage;
                        block(
                            s,
                            from,
                            BlockKind::Restart,
                            BLOCK_UNFINISHED,
                            "the report could not be read".into(),
                        )
                    }
                    ("bind", ActivityResult::Git(GitResult::Sha(tip))) => {
                        let from = s.stage;
                        let claimed = s.claimed.clone().unwrap_or_default();
                        if !tip.eq_ignore_ascii_case(&claimed) {
                            let why = format!(
                                "claimed sha {claimed} is not the tip {tip} of {}",
                                s.input.branch
                            );
                            return block(s, from, BlockKind::Restart, BLOCK_BAD_SHA, why);
                        }
                        s.candidate = Some(tip);
                        match from {
                            Stage::Fix => {
                                let next = s.round + 1;
                                enter_review(s, next)
                            }
                            _ => spawn_reviewer(s),
                        }
                    }
                    ("gate", ActivityResult::Git(GitResult::Sha(tip))) => {
                        let reviewed = s.reviewed.clone().unwrap_or_default();
                        if !tip.eq_ignore_ascii_case(&reviewed) {
                            let why = format!(
                                "acceptance refused: {} moved after review (reviewed {reviewed}, tip now {tip})",
                                s.input.branch
                            );
                            let d = block(s, Stage::Review, BlockKind::Moved, BLOCK_SHA_MOVED, why);
                            if let Some(b) = s.blocked.as_mut() {
                                b.moved_to = Some(tip);
                            }
                            return d;
                        }
                        s.stage = Stage::Merge;
                        // The merge takes the reviewed sha, never the branch name.
                        activity(
                            "merge",
                            ActivityRequest::Git(GitRequest::Merge { branch: reviewed }),
                        )
                    }
                    ("merge", ActivityResult::Git(GitResult::Merged { sha })) => {
                        done("merged", s, Some(sha))
                    }
                    ("merge", ActivityResult::Git(GitResult::Sha(why))) => {
                        block(s, Stage::Gate, BlockKind::Restart, BLOCK_MERGE_REFUSED, why)
                    }
                    ("bind" | "gate" | "merge", _) => {
                        let from = if s.stage == Stage::Merge {
                            Stage::Gate
                        } else {
                            s.stage
                        };
                        block(
                            s,
                            from,
                            BlockKind::Restart,
                            BLOCK_UNFINISHED,
                            "git returned an unexpected result".into(),
                        )
                    }
                    _ => StepDecision::Continue,
                }
            }
            _ => StepDecision::Continue,
        }
    }
}

// ---------------------------------------------------------------------------
// Report reading. Pure text functions: the flow reads reports through the
// ReportFile activity and hands the text here.
// ---------------------------------------------------------------------------

fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(name))
        .map(str::trim)
}

fn has_challenge(text: &str) -> bool {
    text.lines().any(|l| l.trim().starts_with("## Challenge"))
}

fn outcome_is_no_changes(text: &str) -> bool {
    let v = field(text, "Outcome:").unwrap_or("");
    v.split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
        .eq_ignore_ascii_case("NO_CHANGES")
}

/// A report that declares itself unfinished or lists real blockers.
fn unfinished(text: &str) -> Option<String> {
    let outcome = field(text, "Outcome:")
        .and_then(|v| v.split_whitespace().next())
        .unwrap_or("");
    if matches!(
        outcome,
        "BLOCKED" | "DISPUTED" | "REOPEN_REQUEST" | "DEPENDENCY_REQUEST"
    ) {
        return Some(format!("the report ends in {outcome}"));
    }
    let mut in_blockers = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("## ") {
            in_blockers = t.trim_start_matches("## ").eq_ignore_ascii_case("blockers");
            continue;
        }
        if in_blockers && !t.is_empty() && !t.eq_ignore_ascii_case("none") {
            return Some("the report lists blockers".into());
        }
    }
    None
}

const PASSING_LEVELS: [&str; 3] = ["unit-test-verified", "type-check-only", "review-clean"];

/// The sha of a `Verified: <level> <sha>` line with a passing level. The sha
/// is the worker's claim; the flow binds it to git afterwards.
fn verified_sha(text: &str) -> Result<String, (i32, String)> {
    let mut parts = field(text, "Verified:").unwrap_or("").split_whitespace();
    let (level, sha) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if !PASSING_LEVELS.contains(&level) {
        return Err((
            BLOCK_NOT_VERIFIED,
            format!("no passing Verified level (got '{level}')"),
        ));
    }
    if !crate::merge::is_full_sha(sha) {
        return Err((
            BLOCK_BAD_SHA,
            format!("'{sha}' is not a full 40-hex candidate sha"),
        ));
    }
    Ok(sha.to_string())
}

/// `Verdict: clean` or `Verdict: <N> findings`; the last verdict line wins and
/// anything else fails closed.
fn parse_verdict(text: &str) -> Result<u32, String> {
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|l| l.starts_with("Verdict:") || l.starts_with("verdict:"))
        .ok_or("review report has no 'Verdict: clean | <N> findings' line")?;
    let rest = line.split_once(':').map(|(_, r)| r.trim()).unwrap_or("");
    if rest.eq_ignore_ascii_case("clean") {
        return Ok(0);
    }
    let mut w = rest.split_whitespace();
    if let (Some(n), Some(noun), None) = (w.next(), w.next(), w.next()) {
        let noun = noun.trim_matches(|c: char| !c.is_alphanumeric());
        if let Ok(n) = n.parse::<u32>() {
            if noun.eq_ignore_ascii_case("findings") || noun.eq_ignore_ascii_case("finding") {
                return Ok(n);
            }
        }
    }
    Err(format!("unparseable verdict line '{line}'"))
}

fn severity(tag: &str) -> Option<u8> {
    match tag.to_ascii_lowercase().as_str() {
        "critical" => Some(3),
        "major" => Some(2),
        "minor" => Some(1),
        "trivial" => Some(0),
        _ => None,
    }
}

/// Findings that block acceptance: tagged at or above the brief's
/// `Must fix:` (default major), plus every finding of the verdict count that
/// carries no tag.
fn blocking_findings(brief: &str, report: &str, verdict_count: u32) -> u32 {
    let must = field(brief, "Must fix:")
        .and_then(|v| v.split_whitespace().next())
        .and_then(severity)
        .unwrap_or(2);
    let tags: Vec<u8> = report
        .lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("- [")?;
            severity(rest.split_once(']')?.0)
        })
        .collect();
    let tagged_blocking = tags.iter().filter(|t| **t >= must).count() as u32;
    let untagged = verdict_count.saturating_sub(tags.len() as u32);
    tagged_blocking + untagged
}

/// The input of a flow, from the JSON the verb journals.
pub fn input_from(v: &Value) -> Result<PipelineInput, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123abc4567def890123abc4567def890123abcd";

    #[test]
    fn verdicts_fail_closed() {
        assert_eq!(parse_verdict("Verdict: clean\n"), Ok(0));
        assert_eq!(parse_verdict("Verdict: 3 findings"), Ok(3));
        assert!(parse_verdict("Verdict: clean, mostly").is_err());
        assert!(parse_verdict("looks fine").is_err());
    }

    #[test]
    fn severity_gates_blocking_findings() {
        let report = "- [major] a\n- [trivial] b\n- [minor] c\nVerdict: 3 findings";
        assert_eq!(blocking_findings("brief", report, 3), 1);
        assert_eq!(blocking_findings("Must fix: minor", report, 3), 2);
        // untagged findings of the count all block
        assert_eq!(blocking_findings("brief", "Verdict: 2 findings", 2), 2);
    }

    #[test]
    fn verified_needs_a_passing_level_and_a_full_sha() {
        assert_eq!(
            verified_sha(&format!("Verified: unit-test-verified {SHA}")),
            Ok(SHA.into())
        );
        assert!(verified_sha(&format!("Verified: inconclusive {SHA}")).is_err());
        assert!(verified_sha("Verified: unit-test-verified abc1234").is_err());
    }
}
