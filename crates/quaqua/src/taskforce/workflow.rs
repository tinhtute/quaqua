//! Taskforce workflow definition supporting advisor, dual-lens, and council modes.

use crate::engine::child::{SingleWorkerInput, SingleWorkerOutput};
use crate::engine::workflow::{StepDecision, Workflow, WorkflowContext, WorkflowEvent};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Canonical child workflow type string for single worker tasks.
pub const WORKFLOW_TYPE_SINGLE_WORKER: &str = "single_worker";

/// Input contract for starting a taskforce workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskforceInput {
    pub mode: String,
    pub task: String,
    pub repo: String,
    pub brief: String,
    pub members: Vec<SingleWorkerInput>,
    #[serde(default)]
    pub quorum: Option<usize>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub chair_owner: Option<String>,
}

impl TaskforceInput {
    pub fn for_advisor(
        task: impl Into<String>,
        repo: impl Into<String>,
        brief: impl Into<String>,
        member: SingleWorkerInput,
    ) -> Self {
        Self {
            mode: "advisor".to_string(),
            task: task.into(),
            repo: repo.into(),
            brief: brief.into(),
            members: vec![member],
            quorum: Some(1),
            role: Some("advisor".to_string()),
            chair_owner: None,
        }
    }

    pub fn for_dual_lens(
        task: impl Into<String>,
        repo: impl Into<String>,
        brief: impl Into<String>,
        member1: SingleWorkerInput,
        member2: SingleWorkerInput,
    ) -> Self {
        Self {
            mode: "dual-lens".to_string(),
            task: task.into(),
            repo: repo.into(),
            brief: brief.into(),
            members: vec![member1, member2],
            quorum: Some(2),
            role: Some("dual-lens".to_string()),
            chair_owner: None,
        }
    }

    pub fn for_council(
        task: impl Into<String>,
        repo: impl Into<String>,
        brief: impl Into<String>,
        members: Vec<SingleWorkerInput>,
        quorum: Option<usize>,
        role: Option<String>,
        chair_owner: Option<String>,
    ) -> Self {
        Self {
            mode: "council".to_string(),
            task: task.into(),
            repo: repo.into(),
            brief: brief.into(),
            members,
            quorum,
            role,
            chair_owner,
        }
    }
}

/// Lifecycle status for an individual taskforce child member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MemberStatus {
    Pending,
    Started,
    Completed {
        report_path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verdict: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        findings_count: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_challenge: Option<bool>,
    },
    Failed {
        code: i32,
        reason: String,
    },
}

/// Tracking state for a single member slot in the taskforce workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberSlot {
    pub id: String,
    pub input: SingleWorkerInput,
    pub status: MemberStatus,
}

/// Persistent state machine snapshot for [`TaskforceWorkflow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskforceState {
    pub mode: String,
    pub task: String,
    pub members: Vec<MemberSlot>,
    pub started: bool,
    #[serde(default)]
    pub quorum: usize,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub chair_owner: String,
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub merged_report: Option<PathBuf>,
}

impl TaskforceState {
    pub fn from_input(input: &TaskforceInput) -> Self {
        let total = input.members.len();
        let quorum = input.quorum.unwrap_or_else(|| {
            if input.mode == "dual-lens" {
                2
            } else if input.mode == "advisor" {
                1
            } else if total > 0 {
                total / 2 + 1
            } else {
                1
            }
        });
        Self {
            mode: input.mode.clone(),
            task: input.task.clone(),
            role: input.role.clone().unwrap_or_else(|| input.mode.clone()),
            chair_owner: input
                .chair_owner
                .clone()
                .unwrap_or_else(|| "cos".to_string()),
            quorum,
            members: input
                .members
                .iter()
                .map(|m| MemberSlot {
                    id: m.name.clone(),
                    input: m.clone(),
                    status: MemberStatus::Pending,
                })
                .collect(),
            started: false,
            finished: false,
            merged_report: None,
        }
    }
}

/// Terminal output returned by a completed [`TaskforceWorkflow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TaskforceOutput {
    pub report_paths: Vec<PathBuf>,
    #[serde(default)]
    pub quorum_met: bool,
    #[serde(default)]
    pub tally: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub majority: Option<String>,
    #[serde(default)]
    pub dissent: Vec<String>,
    #[serde(default)]
    pub failed: Vec<String>,
    #[serde(default)]
    pub merged_report: Option<PathBuf>,
}

type ChildCompletion = (PathBuf, Option<String>, Option<u32>, Option<bool>);

fn parse_child_completion(
    slot: &MemberSlot,
    output: &serde_json::Value,
) -> Result<ChildCompletion, (i32, String)> {
    let worker_out: SingleWorkerOutput = serde_json::from_value(output.clone()).map_err(|e| {
        (
            6,
            format!(
                "taskforce member '{}' returned malformed child output (expected SingleWorkerOutput): {e}",
                slot.id
            ),
        )
    })?;

    if worker_out.exit != 0 {
        return Err((
            worker_out.exit,
            format!(
                "taskforce member '{}' exited with code {}",
                slot.id, worker_out.exit
            ),
        ));
    }

    let verdict = output
        .get("verdict")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let findings = output
        .get("findings")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let is_challenge = output.get("is_challenge").and_then(|v| v.as_bool());

    Ok((worker_out.report_path, verdict, findings, is_challenge))
}

pub fn build_taskforce_output(state: &TaskforceState) -> TaskforceOutput {
    let mut report_paths = Vec::new();
    let mut tally: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut failed = Vec::new();

    for m in &state.members {
        match &m.status {
            MemberStatus::Completed {
                report_path,
                verdict,
                ..
            } => {
                report_paths.push(report_path.clone());
                if let Some(v) = verdict {
                    tally.entry(v.clone()).or_default().push(m.id.clone());
                } else {
                    tally
                        .entry("unspecified".to_string())
                        .or_default()
                        .push(m.id.clone());
                }
            }
            MemberStatus::Failed { .. } => {
                failed.push(m.id.clone());
            }
            _ => {}
        }
    }

    let completed_count = report_paths.len();
    let quorum_met = completed_count >= state.quorum;

    let mut majority = None;
    let mut dissent = Vec::new();
    if completed_count > 0 {
        for (v, ids) in &tally {
            if ids.len() > completed_count / 2 {
                majority = Some(v.clone());
                break;
            }
        }
        if let Some(maj) = &majority {
            for (v, ids) in &tally {
                if v != maj {
                    dissent.extend(ids.clone());
                }
            }
        } else if tally.len() > 1 {
            for ids in tally.values() {
                dissent.extend(ids.clone());
            }
        }
    }
    TaskforceOutput {
        report_paths,
        quorum_met,
        tally,
        majority,
        dissent,
        failed,
        merged_report: state.merged_report.clone(),
    }
}

/// Deterministic workflow orchestrating taskforce child workers.
#[derive(Debug, Clone, Default)]
pub struct TaskforceWorkflow;

impl TaskforceWorkflow {
    fn evaluate_progress(&self, state: &mut TaskforceState) -> StepDecision<TaskforceOutput> {
        let n = state.members.len();
        let completed_count = state
            .members
            .iter()
            .filter(|m| matches!(m.status, MemberStatus::Completed { .. }))
            .count();
        let failed_count = state
            .members
            .iter()
            .filter(|m| matches!(m.status, MemberStatus::Failed { .. }))
            .count();

        // Fail-fast condition: quorum is mathematically unreachable
        if n - failed_count < state.quorum {
            state.finished = true;
            let (code, reason) = if state.mode == "council" {
                (
                    3,
                    format!(
                        "quorum unreachable: {failed_count} of {n} members failed (quorum {})",
                        state.quorum
                    ),
                )
            } else {
                let first_fail = state.members.iter().find_map(|m| match &m.status {
                    MemberStatus::Failed { code, reason } => Some((*code, reason.clone())),
                    _ => None,
                });
                first_fail.unwrap_or((1, "taskforce member failed".to_string()))
            };
            return StepDecision::Fail { code, reason };
        }

        // Completion condition: all members finished
        if completed_count + failed_count == n {
            state.finished = true;
            let output = build_taskforce_output(state);
            StepDecision::Complete(output)
        } else {
            StepDecision::Continue
        }
    }
}

impl Workflow for TaskforceWorkflow {
    type Input = TaskforceInput;
    type State = TaskforceState;
    type Output = TaskforceOutput;

    fn step(
        &self,
        state: &mut Self::State,
        ctx: &mut WorkflowContext,
        event: Option<WorkflowEvent>,
    ) -> StepDecision<Self::Output> {
        match event {
            None => {
                if !state.started {
                    state.started = true;
                    match state.mode.as_str() {
                        "advisor" => {
                            if state.members.len() != 1 {
                                return StepDecision::Fail {
                                    code: 1,
                                    reason: format!(
                                        "advisor mode requires exactly 1 member, got {}",
                                        state.members.len()
                                    ),
                                };
                            }
                        }
                        "dual-lens" => {
                            if state.members.len() != 2 {
                                return StepDecision::Fail {
                                    code: 1,
                                    reason: format!(
                                        "dual-lens mode requires exactly 2 members, got {}",
                                        state.members.len()
                                    ),
                                };
                            }
                        }
                        "council" => {
                            let n = state.members.len();
                            if !(1..=5).contains(&n) {
                                return StepDecision::Fail {
                                    code: 1,
                                    reason: format!(
                                        "council mode requires between 1 and 5 members, got {n}"
                                    ),
                                };
                            }
                            if state.quorum == 0 || state.quorum > n {
                                return StepDecision::Fail {
                                    code: 1,
                                    reason: format!(
                                        "council quorum {} must be in 1..={n}",
                                        state.quorum
                                    ),
                                };
                            }
                        }
                        other => {
                            return StepDecision::Fail {
                                code: 1,
                                reason: format!("unknown taskforce mode: {other}"),
                            };
                        }
                    }
                    for member in &mut state.members {
                        member.status = MemberStatus::Started;
                        let child_val = match serde_json::to_value(&member.input) {
                            Ok(v) => v,
                            Err(e) => {
                                return StepDecision::Fail {
                                    code: 1,
                                    reason: format!("failed to serialize child input: {e}"),
                                };
                            }
                        };
                        ctx.start_child(&member.id, WORKFLOW_TYPE_SINGLE_WORKER, child_val);
                    }
                }
                StepDecision::Continue
            }

            Some(WorkflowEvent::ChildCompleted { id, output }) => {
                let member_idx = state.members.iter().position(|m| m.id == id);
                let idx = match member_idx {
                    Some(i) => i,
                    None => {
                        return StepDecision::Fail {
                            code: 1,
                            reason: format!("unknown child workflow completion: {id}"),
                        };
                    }
                };

                let outcome = parse_child_completion(&state.members[idx], &output);
                match outcome {
                    Ok((report_path, verdict, findings_count, is_challenge)) => {
                        state.members[idx].status = MemberStatus::Completed {
                            report_path,
                            verdict,
                            findings_count,
                            is_challenge,
                        };
                    }
                    Err((code, reason)) => {
                        state.members[idx].status = MemberStatus::Failed {
                            code,
                            reason: reason.clone(),
                        };
                    }
                }

                self.evaluate_progress(state)
            }

            Some(WorkflowEvent::ChildFailed { id, code, reason }) => {
                let member_idx = state.members.iter().position(|m| m.id == id);
                let idx = match member_idx {
                    Some(i) => i,
                    None => {
                        return StepDecision::Fail {
                            code: 1,
                            reason: format!("unknown child workflow failure: {id}"),
                        };
                    }
                };
                state.members[idx].status = MemberStatus::Failed {
                    code,
                    reason: reason.clone(),
                };

                self.evaluate_progress(state)
            }

            Some(unexpected) => StepDecision::Fail {
                code: 1,
                reason: format!("unexpected event in taskforce workflow: {unexpected:?}"),
            },
        }
    }
}
