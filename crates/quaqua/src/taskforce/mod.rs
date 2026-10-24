//! Taskforce subcommands (`quaqua taskforce {advisor, dual-lens}`).
//! Reproduces piggery taskforce behavior.

use crate::cap::count_owner_slots;
use crate::engine::child::{SingleWorkerInput, SingleWorkerOutput};
use crate::engine::workflow::{StepDecision, Workflow, WorkflowContext, WorkflowEvent};
use crate::git::Git;
use crate::herdr::Herdr;
use crate::worker::cleanup::default_runs_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "\
# Usage: quaqua taskforce <advisor|dual-lens> <task> <repo> --brief <file> [--base <ref>] [--approve-kind <kind>...]
#   --approve-kind <kind>  approve a member kind whose team.toml spawn = \"approve\"";

pub struct TaskforceArgs {
    pub mode: String,
    pub task: String,
    pub repo: PathBuf,
    pub brief: PathBuf,
    pub base: Option<String>,
    pub approve_kinds: Vec<String>,
}

pub fn parse_taskforce_args(args: &[String]) -> Result<TaskforceArgs, (i32, String)> {
    if args.is_empty() {
        return Err((1, format!("{}\n", USAGE)));
    }
    let mode = args[0].clone();
    if mode != "advisor" && mode != "dual-lens" {
        return Err((1, format!("{}\n", USAGE)));
    }

    let mut brief: Option<PathBuf> = None;
    let mut base: Option<String> = None;
    let mut approve_kinds: Vec<String> = Vec::new();
    let mut pos = Vec::new();

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--brief" {
            if i + 1 >= args.len() {
                return Err((1, format!("{}\n", USAGE)));
            }
            brief = Some(PathBuf::from(&args[i + 1]));
            i += 2;
        } else if let Some(v) = arg.strip_prefix("--brief=") {
            brief = Some(PathBuf::from(v));
            i += 1;
        } else if arg == "--approve-kind" {
            if i + 1 >= args.len() {
                return Err((1, format!("{}\n", USAGE)));
            }
            let val = &args[i + 1];
            if val.is_empty() || val.starts_with('-') {
                return Err((1, format!("{}\n", USAGE)));
            }
            approve_kinds.push(val.clone());
            i += 2;
        } else if let Some(v) = arg.strip_prefix("--approve-kind=") {
            if v.is_empty() || v.starts_with('-') {
                return Err((1, format!("{}\n", USAGE)));
            }
            approve_kinds.push(v.to_string());
            i += 1;
        } else if arg == "--base" {
            if i + 1 >= args.len() {
                return Err((1, format!("{}\n", USAGE)));
            }
            base = Some(args[i + 1].clone());
            i += 2;
        } else if let Some(v) = arg.strip_prefix("--base=") {
            base = Some(v.to_string());
            i += 1;
        } else if arg.starts_with('-') {
            return Err((1, format!("{}\n", USAGE)));
        } else {
            pos.push(arg.clone());
            i += 1;
        }
    }

    if pos.len() < 2 || brief.is_none() {
        return Err((1, format!("{}\n", USAGE)));
    }

    Ok(TaskforceArgs {
        mode,
        task: pos[0].clone(),
        repo: PathBuf::from(&pos[1]),
        brief: brief.unwrap(),
        base,
        approve_kinds,
    })
}

pub fn handle_taskforce(args: &[String], git: &Git, herdr: &Herdr) -> Result<(), (i32, String)> {
    let targs = parse_taskforce_args(args)?;
    let owner = std::env::var("QUAQUA_OWNER").unwrap_or_else(|_| "cos".to_string());
    let runs_dir = default_runs_dir();
    let _ = std::fs::create_dir_all(&runs_dir);

    let base_cfg = crate::config::load_team_config()?;
    let proj_slug = crate::pipeline::resolve_project_slug_from_repo(&targs.repo);
    let cfg = if let Some(slug) = proj_slug.as_deref() {
        crate::config::resolve(base_cfg.clone(), Some(slug)).unwrap_or(base_cfg)
    } else {
        base_cfg
    };
    crate::layout::shadow(None, Some(herdr), crate::layout::Verb::Spawn, "taskforce", Some(&targs.task), crate::layout::SpawnOrigin::Cli, Some(&owner), proj_slug.as_deref(), &runs_dir, Some(&cfg));
    let weight = cfg
        .limits
        .weights
        .get(&targs.mode)
        .copied()
        .unwrap_or(if targs.mode == "dual-lens" { 2 } else { 1 });
    let slots_used = count_owner_slots(&runs_dir, &owner, None, herdr, &cfg);
    crate::cap::check_owner_cap(&cfg, &owner, slots_used, weight, &targs.mode)
        .map_err(|e| (2, format!("error: {e}\n")))?;

    let brief_content = std::fs::read_to_string(&targs.brief).map_err(|e| {
        (
            1,
            format!("cannot read brief {}: {e}\n", targs.brief.display()),
        )
    })?;

    if targs.mode == "advisor" {
        run_advisor(&targs, &brief_content, &runs_dir, &owner, &cfg, git, herdr)
    } else {
        run_dual_lens(&targs, &brief_content, &runs_dir, &owner, &cfg, git, herdr)
    }
}

fn run_advisor(
    args: &TaskforceArgs,
    brief: &str,
    runs_dir: &Path,
    owner: &str,
    cfg: &crate::config::Config,
    git: &Git,
    herdr: &Herdr,
) -> Result<(), (i32, String)> {
    let default_advisor = cfg
        .purposes
        .get("review")
        .or_else(|| cfg.strategy.default_reviewer.first())
        .unwrap_or(&cfg.strategy.default_kind)
        .clone();
    let members = cfg
        .taskforce
        .get("advisor")
        .map(|t| t.members.clone())
        .unwrap_or_else(|| vec![default_advisor.clone()]);
    let kind = members.first().map(|s| s.as_str()).unwrap_or(&default_advisor);
    let worker_name = format!("{kind}-{}", args.task);

    let opts = crate::worker::spawn::SpawnOptions {
        base: args.base.clone(),
        review: true,
        name: worker_name.clone(),
        kind: Some(kind.to_string()),
        purpose: None,
        repo_path: args.repo.clone(),
        approve_kinds: args.approve_kinds.clone(),
        solo: None,
    };

    // Set PIPELINE_TASK during inner spawn to prevent double-charging the reservation
    std::env::set_var("PIPELINE_TASK", &args.task);
    std::env::set_var("QUAQUA_OWNER", owner);
    let spawn_res = crate::worker::spawn::spawn_worker(opts, cfg, git, herdr, crate::layout::SpawnOrigin::TaskforceMember);
    std::env::remove_var("PIPELINE_TASK");
    spawn_res?;

    // Write taskforce marker
    let tf_marker = runs_dir.join(format!("{worker_name}.taskforce"));
    let _ = std::fs::write(&tf_marker, &args.task);

    let prompt_text = format!(
        "Goal: advisor recommendation for task '{}'\nHard constraints: read-only — do not edit or create files outside ~/quaqua/runs.\nCurrent design (open to challenge): {}\nEvidence: recommendation + confidence, evidence, alternatives, strongest downside, and the smallest check that would prove it wrong (marked not run). Write to a temp file and rename it to ~/quaqua/runs/{worker_name}.md — publish atomically, never a partial file.",
        args.task, brief
    );

    let prompt_res = crate::worker::prompt::prompt_worker(
        &worker_name,
        Some(&prompt_text),
        false,
        None,
        git,
        herdr,
    );
    let report_path = runs_dir.join(format!("{worker_name}.md"));
    if prompt_res.is_ok() {
        println!("{}", report_path.display());
    }
    prompt_res
}

fn run_dual_lens(
    args: &TaskforceArgs,
    brief: &str,
    runs_dir: &Path,
    owner: &str,
    cfg: &crate::config::Config,
    git: &Git,
    herdr: &Herdr,
) -> Result<(), (i32, String)> {
    let default_lens1 = cfg
        .purposes
        .get("design")
        .or_else(|| cfg.purposes.get("research"))
        .unwrap_or(&cfg.strategy.default_kind)
        .clone();
    let default_lens2 = cfg
        .purposes
        .get("review")
        .or_else(|| cfg.strategy.default_reviewer.first())
        .unwrap_or(&cfg.strategy.default_kind)
        .clone();
    let members = cfg
        .taskforce
        .get("dual-lens")
        .map(|t| t.members.clone())
        .unwrap_or_else(|| vec![default_lens1.clone(), default_lens2.clone()]);
    let kind1 = members.first().map(|s| s.as_str()).unwrap_or(&default_lens1);
    let kind2 = members.get(1).map(|s| s.as_str()).unwrap_or(&default_lens2);

    let w1 = format!("{kind1}-{}dl1", args.task);
    let w2 = format!("{kind2}-{}dl2", args.task);

    let state_file = runs_dir.join(format!("taskforce-{}.state", args.task));
    let pid = std::process::id();
    let state_content = format!(
        "task={}\nkind=dual-lens\nowner={}\nfinished=0\ntaskforce_pid={}\n",
        args.task, owner, pid
    );
    let _ = std::fs::write(&state_file, state_content);

    let tf1 = runs_dir.join(format!("{w1}.taskforce"));
    let tf2 = runs_dir.join(format!("{w2}.taskforce"));
    let _ = std::fs::write(&tf1, &args.task);
    let _ = std::fs::write(&tf2, &args.task);

    let opts1 = crate::worker::spawn::SpawnOptions {
        base: args.base.clone(),
        review: true,
        name: w1.clone(),
        kind: Some(kind1.to_string()),
        purpose: None,
        repo_path: args.repo.clone(),
        approve_kinds: args.approve_kinds.clone(),
        solo: None,
    };

    let opts2 = crate::worker::spawn::SpawnOptions {
        base: args.base.clone(),
        review: true,
        name: w2.clone(),
        kind: Some(kind2.to_string()),
        purpose: None,
        repo_path: args.repo.clone(),
        approve_kinds: args.approve_kinds.clone(),
        solo: None,
    };

    std::env::set_var("PIPELINE_TASK", &args.task);
    std::env::set_var("QUAQUA_OWNER", owner);
    let s1 = crate::worker::spawn::spawn_worker(opts1, cfg, git, herdr, crate::layout::SpawnOrigin::TaskforceMember);
    let s2 = if s1.is_ok() {
        crate::worker::spawn::spawn_worker(opts2, cfg, git, herdr, crate::layout::SpawnOrigin::TaskforceMember)
    } else {
        Ok(())
    };
    std::env::remove_var("PIPELINE_TASK");

    if let Err(e) = s1 {
        let _ = std::fs::write(
            &state_file,
            format!(
                "task={}\nkind=dual-lens\nowner={}\nfinished=1\n",
                args.task, owner
            ),
        );
        return Err(e);
    }
    if let Err(e) = s2 {
        let _ = crate::worker::cleanup::cleanup_worker(&w1, &args.repo, git, herdr);
        let _ = std::fs::write(
            &state_file,
            format!(
                "task={}\nkind=dual-lens\nowner={}\nfinished=1\n",
                args.task, owner
            ),
        );
        return Err(e);
    }

    let prompt1 = format!(
        "Goal: dual-lens assessment (lens 1) for task '{}'\nHard constraints: read-only — do not edit or create files outside ~/quaqua/runs.\nCurrent design (open to challenge): {}\nEvidence: assessment, agreements, and concerns. Write to a temp file and rename it to ~/quaqua/runs/{w1}.md — publish atomically, never a partial file.",
        args.task, brief
    );
    let prompt2 = format!(
        "Goal: dual-lens assessment (lens 2) for task '{}'\nHard constraints: read-only — do not edit or create files outside ~/quaqua/runs.\nCurrent design (open to challenge): {}\nEvidence: assessment, agreements, and concerns. Write to a temp file and rename it to ~/quaqua/runs/{w2}.md — publish atomically, never a partial file.",
        args.task, brief
    );

    let git1 = git.clone();
    let herdr1 = herdr.clone();
    let w1_c = w1.clone();
    let prompt1_c = prompt1.clone();
    let handle1 = std::thread::spawn(move || {
        crate::worker::prompt::prompt_worker(&w1_c, Some(&prompt1_c), false, None, &git1, &herdr1)
    });

    let git2 = git.clone();
    let herdr2 = herdr.clone();
    let w2_c = w2.clone();
    let prompt2_c = prompt2.clone();
    let handle2 = std::thread::spawn(move || {
        crate::worker::prompt::prompt_worker(&w2_c, Some(&prompt2_c), false, None, &git2, &herdr2)
    });

    let p1_res = handle1
        .join()
        .map_err(|_| (1, "lens 1 thread failed\n".to_string()))?;
    let p2_res = handle2
        .join()
        .map_err(|_| (1, "lens 2 thread failed\n".to_string()))?;

    let _ = std::fs::write(
        &state_file,
        format!(
            "task={}\nkind=dual-lens\nowner={}\nfinished=1\ntaskforce_pid={}\n",
            args.task, owner, pid
        ),
    );

    p1_res?;
    p2_res?;

    println!("{}", runs_dir.join(format!("{w1}.md")).display());
    println!("{}", runs_dir.join(format!("{w2}.md")).display());
    Ok(())
}

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
        }
    }
}

/// Lifecycle status for an individual taskforce child member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MemberStatus {
    Pending,
    Started,
    Completed { report_path: PathBuf },
    Failed { code: i32, reason: String },
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
}

impl TaskforceState {
    pub fn from_input(input: &TaskforceInput) -> Self {
        Self {
            mode: input.mode.clone(),
            task: input.task.clone(),
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
        }
    }
}

/// Terminal output returned by a completed [`TaskforceWorkflow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskforceOutput {
    pub report_paths: Vec<PathBuf>,
}

fn parse_child_completion(
    slot: &MemberSlot,
    output: &serde_json::Value,
) -> Result<PathBuf, (i32, String)> {
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

    Ok(worker_out.report_path)
}

/// Deterministic workflow orchestrating taskforce child workers.
#[derive(Debug, Clone, Default)]
pub struct TaskforceWorkflow;

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
                    Ok(report_path) => {
                        state.members[idx].status = MemberStatus::Completed { report_path };
                    }
                    Err((code, reason)) => {
                        state.members[idx].status = MemberStatus::Failed {
                            code,
                            reason: reason.clone(),
                        };
                        return StepDecision::Fail { code, reason };
                    }
                }

                // Quorum check: complete when every member is Completed
                if state
                    .members
                    .iter()
                    .all(|m| matches!(m.status, MemberStatus::Completed { .. }))
                {
                    let report_paths = state
                        .members
                        .iter()
                        .filter_map(|m| match &m.status {
                            MemberStatus::Completed { report_path } => Some(report_path.clone()),
                            _ => None,
                        })
                        .collect();
                    StepDecision::Complete(TaskforceOutput { report_paths })
                } else {
                    StepDecision::Continue
                }
            }

            Some(WorkflowEvent::ChildFailed { id, code, reason }) => {
                if let Some(member) = state.members.iter_mut().find(|m| m.id == id) {
                    member.status = MemberStatus::Failed {
                        code,
                        reason: reason.clone(),
                    };
                }
                // Mirror legacy run_dual_lens: any member failure immediately aborts with that code
                StepDecision::Fail {
                    code,
                    reason: format!("taskforce member '{id}' failed (exit {code}): {reason}"),
                }
            }

            Some(unexpected) => StepDecision::Fail {
                code: 1,
                reason: format!("unexpected event in taskforce workflow: {unexpected:?}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_taskforce_dual_lens_quorum_completes_with_both_report_paths() {
        let workflow = TaskforceWorkflow;
        let member1 = SingleWorkerInput::new(
            "truonglao-mytestdl1",
            "truonglao",
            "/path/to/repo",
            "dual lens assessment",
            2700,
            None,
        );
        let member2 = SingleWorkerInput::new(
            "suhuynh-mytestdl2",
            "suhuynh",
            "/path/to/repo",
            "dual lens assessment",
            2700,
            None,
        );
        let input = TaskforceInput::for_dual_lens(
            "mytest",
            "/path/to/repo",
            "dual lens assessment",
            member1,
            member2,
        );
        let mut state = TaskforceState::from_input(&input);
        let mut ctx = WorkflowContext::new(100);

        // Initial step starts both children concurrently
        let dec_init = workflow.step(&mut state, &mut ctx, None);
        assert_eq!(dec_init, StepDecision::Continue);
        assert_eq!(
            ctx.scheduled().len(),
            2,
            "both dual-lens children should start concurrently"
        );

        // First member completes with exit 0
        let out1 =
            SingleWorkerOutput::new(0, PathBuf::from("runs/truonglao-mytestdl1.md")).unwrap();
        let dec_c1 = workflow.step(
            &mut state,
            &mut ctx,
            Some(WorkflowEvent::ChildCompleted {
                id: "truonglao-mytestdl1".to_string(),
                output: serde_json::to_value(&out1).unwrap(),
            }),
        );
        assert_eq!(
            dec_c1,
            StepDecision::Continue,
            "workflow must await full quorum"
        );
        assert_eq!(
            state.members[0].status,
            MemberStatus::Completed {
                report_path: PathBuf::from("runs/truonglao-mytestdl1.md")
            }
        );

        // Second member completes with exit 0
        let out2 = SingleWorkerOutput::new(0, PathBuf::from("runs/suhuynh-mytestdl2.md")).unwrap();
        let dec_c2 = workflow.step(
            &mut state,
            &mut ctx,
            Some(WorkflowEvent::ChildCompleted {
                id: "suhuynh-mytestdl2".to_string(),
                output: serde_json::to_value(&out2).unwrap(),
            }),
        );

        match dec_c2 {
            StepDecision::Complete(output) => {
                assert_eq!(
                    output.report_paths,
                    vec![
                        PathBuf::from("runs/truonglao-mytestdl1.md"),
                        PathBuf::from("runs/suhuynh-mytestdl2.md")
                    ],
                    "dual-lens quorum must yield both report paths in member order"
                );
            }
            other => panic!("expected StepDecision::Complete, got {other:?}"),
        }
    }

    #[test]
    fn test_taskforce_dual_lens_one_fail_drives_step_directly_with_child_failed() {
        let workflow = TaskforceWorkflow;
        let member1 = SingleWorkerInput::new(
            "truonglao-failtestdl1",
            "truonglao",
            "/path/to/repo",
            "dual lens assessment",
            2700,
            None,
        );
        let member2 = SingleWorkerInput::new(
            "suhuynh-failtestdl2",
            "suhuynh",
            "/path/to/repo",
            "dual lens assessment",
            2700,
            None,
        );
        let input = TaskforceInput::for_dual_lens(
            "failtest",
            "/path/to/repo",
            "dual lens assessment",
            member1,
            member2,
        );
        let mut state = TaskforceState::from_input(&input);
        let mut ctx = WorkflowContext::new(100);

        // Initial step: schedules both children and returns Continue
        let dec_init = workflow.step(&mut state, &mut ctx, None);
        assert_eq!(dec_init, StepDecision::Continue);
        assert_eq!(
            ctx.scheduled().len(),
            2,
            "both dual-lens children should start concurrently"
        );

        // Child 1 completes successfully
        let out1 =
            SingleWorkerOutput::new(0, PathBuf::from("runs/truonglao-failtestdl1.md")).unwrap();
        let dec_c1 = workflow.step(
            &mut state,
            &mut ctx,
            Some(WorkflowEvent::ChildCompleted {
                id: "truonglao-failtestdl1".to_string(),
                output: serde_json::to_value(&out1).unwrap(),
            }),
        );
        assert_eq!(
            dec_c1,
            StepDecision::Continue,
            "workflow must wait for quorum"
        );
        assert_eq!(
            state.members[0].status,
            MemberStatus::Completed {
                report_path: PathBuf::from("runs/truonglao-failtestdl1.md")
            }
        );

        // Child 2 fails: drive step() directly with ChildFailed per contract
        let dec_c2 = workflow.step(
            &mut state,
            &mut ctx,
            Some(WorkflowEvent::ChildFailed {
                id: "suhuynh-failtestdl2".to_string(),
                code: 4,
                reason: "time budget exceeded".to_string(),
            }),
        );

        match dec_c2 {
            StepDecision::Fail { code, reason } => {
                assert_eq!(code, 4, "failure code must match ChildFailed code");
                assert!(
                    reason.contains("time budget exceeded"),
                    "failure reason must propagate child reason"
                );
            }
            other => panic!("expected StepDecision::Fail with code 4, got {other:?}"),
        }
        assert!(matches!(
            state.members[1].status,
            MemberStatus::Failed { code: 4, .. }
        ));
    }

    #[test]
    fn test_taskforce_dual_lens_one_fail_via_child_output_nonzero_exit() {
        let workflow = TaskforceWorkflow;
        let member1 = SingleWorkerInput::new(
            "truonglao-nonzero1",
            "truonglao",
            "/path/to/repo",
            "brief",
            2700,
            None,
        );
        let member2 = SingleWorkerInput::new(
            "suhuynh-nonzero2",
            "suhuynh",
            "/path/to/repo",
            "brief",
            2700,
            None,
        );
        let input =
            TaskforceInput::for_dual_lens("nonzero", "/path/to/repo", "brief", member1, member2);
        let mut state = TaskforceState::from_input(&input);
        let mut ctx = WorkflowContext::new(100);

        let _ = workflow.step(&mut state, &mut ctx, None);

        // Member 1 completes with exit 0
        let out1 = SingleWorkerOutput::new(0, PathBuf::from("runs/truonglao-nonzero1.md")).unwrap();
        let dec_c1 = workflow.step(
            &mut state,
            &mut ctx,
            Some(WorkflowEvent::ChildCompleted {
                id: "truonglao-nonzero1".to_string(),
                output: serde_json::to_value(&out1).unwrap(),
            }),
        );
        assert_eq!(dec_c1, StepDecision::Continue);

        // Member 2 completes with exit 3 (stall)
        let out2 = SingleWorkerOutput::new(3, PathBuf::from("runs/suhuynh-nonzero2.md")).unwrap();
        let dec_c2 = workflow.step(
            &mut state,
            &mut ctx,
            Some(WorkflowEvent::ChildCompleted {
                id: "suhuynh-nonzero2".to_string(),
                output: serde_json::to_value(&out2).unwrap(),
            }),
        );

        match dec_c2 {
            StepDecision::Fail { code, reason } => {
                assert_eq!(code, 3, "failure code must match non-zero child exit code");
                assert!(reason.contains("exited with code 3"));
            }
            other => panic!("expected StepDecision::Fail with code 3, got {other:?}"),
        }
    }

    #[test]
    fn test_taskforce_child_output_malformed_fails_with_code_6() {
        let workflow = TaskforceWorkflow;
        let member = SingleWorkerInput::new("w1", "suhuynh", "repo", "brief", 1000, None);
        let input = TaskforceInput::for_advisor("t1", "repo", "brief", member);
        let mut state = TaskforceState::from_input(&input);
        let mut ctx = WorkflowContext::new(100);

        let _ = workflow.step(&mut state, &mut ctx, None);

        // Malformed empty object output
        let dec_malformed = workflow.step(
            &mut state,
            &mut ctx,
            Some(WorkflowEvent::ChildCompleted {
                id: "w1".to_string(),
                output: serde_json::json!({}),
            }),
        );

        match dec_malformed {
            StepDecision::Fail { code, reason } => {
                assert_eq!(code, 6, "malformed child output must fail with code 6");
                assert!(reason.contains("malformed child output"));
            }
            other => panic!("expected StepDecision::Fail with code 6, got {other:?}"),
        }
    }

    #[test]
    fn test_taskforce_advisor_single_child_path_completes() {
        let workflow = TaskforceWorkflow;
        let member = SingleWorkerInput::new(
            "suhuynh-advtest",
            "suhuynh",
            "/path/to/repo",
            "advisor recommendation",
            2700,
            None,
        );
        let input = TaskforceInput::for_advisor(
            "advtest",
            "/path/to/repo",
            "advisor recommendation",
            member,
        );
        let mut state = TaskforceState::from_input(&input);
        let mut ctx = WorkflowContext::new(100);

        let dec_init = workflow.step(&mut state, &mut ctx, None);
        assert_eq!(dec_init, StepDecision::Continue);
        assert_eq!(ctx.scheduled().len(), 1);

        let out = SingleWorkerOutput::new(0, PathBuf::from("runs/suhuynh-advtest.md")).unwrap();
        let dec_done = workflow.step(
            &mut state,
            &mut ctx,
            Some(WorkflowEvent::ChildCompleted {
                id: "suhuynh-advtest".to_string(),
                output: serde_json::to_value(&out).unwrap(),
            }),
        );

        match dec_done {
            StepDecision::Complete(output) => {
                assert_eq!(
                    output.report_paths,
                    vec![PathBuf::from("runs/suhuynh-advtest.md")],
                    "advisor path must yield exactly one report path"
                );
            }
            other => panic!("expected StepDecision::Complete, got {other:?}"),
        }
    }

    #[test]
    fn test_taskforce_mode_member_count_validation() {
        let workflow = TaskforceWorkflow;
        let m1 = SingleWorkerInput::new("w1", "suhuynh", "repo", "brief", 1000, None);
        let m2 = SingleWorkerInput::new("w2", "truonglao", "repo", "brief", 1000, None);

        // 1. Advisor with 2 members fails
        let input_adv_bad = TaskforceInput {
            mode: "advisor".to_string(),
            task: "t1".to_string(),
            repo: "repo".to_string(),
            brief: "brief".to_string(),
            members: vec![m1.clone(), m2.clone()],
        };
        let mut state_adv_bad = TaskforceState::from_input(&input_adv_bad);
        let mut ctx = WorkflowContext::new(100);
        let dec1 = workflow.step(&mut state_adv_bad, &mut ctx, None);
        match dec1 {
            StepDecision::Fail { code, reason } => {
                assert_eq!(code, 1);
                assert!(reason.contains("advisor mode requires exactly 1 member"));
            }
            other => panic!("expected Fail, got {other:?}"),
        }

        // 2. Dual-lens with 1 member fails
        let input_dl_bad = TaskforceInput {
            mode: "dual-lens".to_string(),
            task: "t2".to_string(),
            repo: "repo".to_string(),
            brief: "brief".to_string(),
            members: vec![m1.clone()],
        };
        let mut state_dl_bad = TaskforceState::from_input(&input_dl_bad);
        let dec2 = workflow.step(&mut state_dl_bad, &mut ctx, None);
        match dec2 {
            StepDecision::Fail { code, reason } => {
                assert_eq!(code, 1);
                assert!(reason.contains("dual-lens mode requires exactly 2 members"));
            }
            other => panic!("expected Fail, got {other:?}"),
        }

        // 3. Unknown mode fails
        let input_unknown = TaskforceInput {
            mode: "trio".to_string(),
            task: "t3".to_string(),
            repo: "repo".to_string(),
            brief: "brief".to_string(),
            members: vec![m1],
        };
        let mut state_unknown = TaskforceState::from_input(&input_unknown);
        let dec3 = workflow.step(&mut state_unknown, &mut ctx, None);
        match dec3 {
            StepDecision::Fail { code, reason } => {
                assert_eq!(code, 1);
                assert!(reason.contains("unknown taskforce mode"));
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn test_taskforce_state_serde_roundtrip() {
        let member = SingleWorkerInput::new("w1", "suhuynh", "repo", "brief", 1000, Some(1.5));
        let input = TaskforceInput::for_advisor("t1", "repo", "brief", member);
        let mut state = TaskforceState::from_input(&input);
        state.started = true;
        state.members[0].status = MemberStatus::Completed {
            report_path: PathBuf::from("runs/w1.md"),
        };

        let serialized = serde_json::to_string(&state).expect("serialize state");
        let deserialized: TaskforceState =
            serde_json::from_str(&serialized).expect("deserialize state");
        assert_eq!(state, deserialized);
    }
    #[test]
    fn approve_kind_flag_is_parsed_and_optional() {
        let args: Vec<String> = [
            "dual-lens",
            "tlcheck",
            ".",
            "--brief",
            "brief.md",
            "--approve-kind",
            "truonglao",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let t = parse_taskforce_args(&args).unwrap();
        assert_eq!(t.approve_kinds, vec!["truonglao".to_string()]);

        let plain: Vec<String> = ["advisor", "tlcheck", ".", "--brief", "brief.md"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(parse_taskforce_args(&plain)
            .unwrap()
            .approve_kinds
            .is_empty());
    }
}
