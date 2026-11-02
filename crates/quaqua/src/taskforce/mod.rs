//! Taskforce subcommands (`quaqua taskforce {advisor, dual-lens, council}`).

pub mod driver;
pub mod report;
pub mod workflow;

pub use driver::{
    build_member_prompt, build_member_prompt_with_neutral, reveal_sealed_reports,
    run_taskforce_workflow, state_file_path, strip_current_design, write_state_file,
    CouncilMemberRunner, LiveCouncilMemberRunner,
};
pub use report::{parse_member_verdict, render_council_report, write_council_report};
pub use workflow::{
    build_taskforce_output, MemberSlot, MemberStatus, TaskforceInput, TaskforceOutput,
    TaskforceState, TaskforceWorkflow, WORKFLOW_TYPE_SINGLE_WORKER,
};

use crate::cap::count_owner_slots;
use crate::git::Git;
use crate::herdr::Herdr;
use crate::worker::cleanup::default_runs_dir;
use std::path::PathBuf;

pub const USAGE: &str = "\
# Usage: quaqua taskforce <advisor|dual-lens|council> <task> <repo> --brief <file> [--base <ref>] [--approve-kind <kind>...]
#   --approve-kind <kind>  approve a member kind whose team.toml spawn = \"approve\"
#
# Note: Taskforce execution is durably event-sourced. A finished or failed task cannot be re-executed
# under the same task name; to rerun or retry, choose a new task name (e.g. <task>-2).";

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
    if mode != "advisor" && mode != "dual-lens" && mode != "council" {
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

/// Resolves council members and quorum:
/// 1. `[taskforce.council]` in team.toml if configured and non-empty
/// 2. Active layout role members if defined
/// 3. Sane default fallback: 3 members (`review`, `review`, `design`)
fn is_can_review(cfg: &crate::config::Config, kind: &str) -> bool {
    if let Some(k) = cfg.kinds.get(kind) {
        k.can_review
    } else {
        true
    }
}

pub fn resolve_council_members(
    cfg: &crate::config::Config,
    role_opt: Option<&crate::layout::LayoutRole>,
    task: &str,
) -> (Vec<(String, String)>, usize) {
    let mut members = Vec::new();
    if let Some(tf) = cfg.taskforce.get("council") {
        for (i, kind) in tf.members.iter().enumerate() {
            let member_name = format!("{kind}-{}c{}", task, i + 1);
            members.push((member_name, kind.clone()));
        }
    }
    if members.is_empty() {
        if let Some(r) = role_opt {
            for (i, m) in r.members.iter().enumerate() {
                let kind = if let Some(p) = &m.purpose {
                    cfg.purposes
                        .get(p)
                        .cloned()
                        .unwrap_or_else(|| cfg.strategy.default_kind.clone())
                } else if let Some(k) = &m.kind {
                    k.clone()
                } else {
                    cfg.strategy.default_kind.clone()
                };
                let member_name = format!("{kind}-{}c{}", task, i + 1);
                members.push((member_name, kind));
            }
        }
    }
    if members.is_empty() {
        // Default fallback: pick up to 3 distinct can_review kinds (or 2 if fewer available)
        let mut candidates: Vec<String> = Vec::new();
        if let Some(r) = cfg.purposes.get("review") {
            if is_can_review(cfg, r) && !candidates.contains(r) {
                candidates.push(r.clone());
            }
        }
        if let Some(d) = cfg.purposes.get("design") {
            if is_can_review(cfg, d) && !candidates.contains(d) {
                candidates.push(d.clone());
            }
        }
        for rev in &cfg.strategy.default_reviewer {
            if is_can_review(cfg, rev) && !candidates.contains(rev) {
                candidates.push(rev.clone());
            }
        }
        for (name, kinfo) in &cfg.kinds {
            if kinfo.can_review && !candidates.contains(name) {
                candidates.push(name.clone());
            }
        }
        if candidates.is_empty() && is_can_review(cfg, &cfg.strategy.default_kind) {
            candidates.push(cfg.strategy.default_kind.clone());
        }

        let count = candidates.len().min(3);
        for (i, kind) in candidates.into_iter().take(count).enumerate() {
            let member_name = format!("{kind}-{}c{}", task, i + 1);
            members.push((member_name, kind));
        }
    }
    let q = role_opt
        .and_then(|r| r.effective_quorum())
        .unwrap_or_else(|| members.len() / 2 + 1);
    (members, q)
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

    let caller_tokens = crate::identity::caller_pane_tokens(herdr);
    let mut caller = crate::identity::resolve_caller_from_tokens(caller_tokens.as_ref(), &runs_dir);
    if caller.role == crate::identity::Role::Unknown
        && std::env::var("QUAQUA_TEST").as_deref() == Ok("1")
        && caller_tokens.is_none()
        && std::env::var("QUAQUA_OWNER").as_deref() != Ok("")
    {
        caller = crate::identity::Caller {
            name: owner.clone(),
            role: if crate::cap::is_lead_owner(&owner) {
                crate::identity::Role::Lead
            } else {
                crate::identity::Role::Cos
            },
            depth: crate::identity::CallerDepth::Resolved(if crate::cap::is_lead_owner(&owner) {
                1
            } else {
                0
            }),
            source: crate::identity::IdentitySource::EnvFallback,
        };
    }

    let (layout, _origin) = crate::layout::resolve_effective_layout(&owner, &runs_dir, &cfg)
        .or_else(|_| {
            crate::layout::resolve_layout(&cfg)
                .map(|l| (l, crate::layout::LayoutOrigin::Live { source: "config" }))
        })
        .map_err(|e| (1, format!("layout error: {e}\n")))?;

    // Gate check: caller -> taskforce role
    crate::layout::gate(
        Some(&caller),
        Some(herdr),
        crate::layout::Verb::Spawn,
        "taskforce",
        Some(&targs.task),
        crate::layout::SpawnOrigin::Cli,
        Some(&owner),
        proj_slug.as_deref(),
        &runs_dir,
        Some(&cfg),
    )?;

    // Resolve specific target role in layout
    let caller_role_name =
        crate::layout::gate::resolve_caller_role(&layout, &caller, Some(&runs_dir)).ok();
    let role_name =
        crate::layout::gate::resolve_target_role(&layout, caller_role_name.as_deref(), &targs.mode)
            .or_else(|_| {
                crate::layout::gate::resolve_target_role(
                    &layout,
                    caller_role_name.as_deref(),
                    "taskforce",
                )
            })
            .unwrap_or(targs.mode.as_str());

    let role_opt = layout.roles.get(role_name);

    // Resolve members and quorum based on mode
    let (member_specs, quorum) = match targs.mode.as_str() {
        "council" => resolve_council_members(&cfg, role_opt, &targs.task),
        "dual-lens" => {
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
            let members_cfg = cfg
                .taskforce
                .get("dual-lens")
                .map(|t| t.members.clone())
                .unwrap_or_else(|| vec![default_lens1.clone(), default_lens2.clone()]);
            let kind1 = members_cfg
                .first()
                .map(|s| s.as_str())
                .unwrap_or(&default_lens1);
            let kind2 = members_cfg
                .get(1)
                .map(|s| s.as_str())
                .unwrap_or(&default_lens2);

            let w1 = format!("{kind1}-{}dl1", targs.task);
            let w2 = format!("{kind2}-{}dl2", targs.task);
            (vec![(w1, kind1.to_string()), (w2, kind2.to_string())], 2)
        }
        _ => {
            // Advisor
            let default_advisor = cfg
                .purposes
                .get("review")
                .or_else(|| cfg.strategy.default_reviewer.first())
                .unwrap_or(&cfg.strategy.default_kind)
                .clone();
            let members_cfg = cfg
                .taskforce
                .get("advisor")
                .map(|t| t.members.clone())
                .unwrap_or_else(|| vec![default_advisor.clone()]);
            let kind = members_cfg
                .first()
                .map(|s| s.as_str())
                .unwrap_or(&default_advisor);
            let w = format!("{kind}-{}", targs.task);
            (vec![(w, kind.to_string())], 1)
        }
    };

    // Validate that all member kinds have can_review = true (F5)
    for (m_name, m_kind) in &member_specs {
        if let Some(kind_info) = cfg.kinds.get(m_kind) {
            if !kind_info.can_review {
                return Err((
                    1,
                    format!(
                        "error: taskforce member '{m_name}' kind '{m_kind}' cannot review (can_review = false in [kinds.{m_kind}]); configure a reviewer kind in [purposes] or the layout's members\n"
                    ),
                ));
            }
        }
    }

    // Calculate effective weight and check cap
    let weight = crate::config::taskforce_weight_from_layout(&cfg, role_name, &layout);
    let slots_used = count_owner_slots(&runs_dir, &owner, None, herdr, &cfg);
    crate::cap::check_owner_cap(&cfg, &owner, slots_used, weight, role_name)
        .map_err(|e| (2, format!("error: {e}\n")))?;

    let brief_content = std::fs::read_to_string(&targs.brief).map_err(|e| {
        (
            1,
            format!("cannot read brief {}: {e}\n", targs.brief.display()),
        )
    })?;

    let live_runner = LiveCouncilMemberRunner {
        git,
        herdr,
        cfg: &cfg,
        base: targs.base.clone(),
    };

    let outcome = run_taskforce_workflow(
        &targs.mode,
        &targs.task,
        &targs.repo,
        &brief_content,
        role_name,
        &member_specs,
        quorum,
        &owner,
        &targs.approve_kinds,
        targs.base.as_deref(),
        &runs_dir,
        &live_runner,
    )?;

    if targs.mode == "council" {
        if let Some(merged) = &outcome.merged_report {
            println!("{}", merged.display());
        }
    }
    for p in &outcome.report_paths {
        println!("{}", p.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::child::{SingleWorkerInput, SingleWorkerOutput};
    use crate::engine::workflow::{StepDecision, Workflow, WorkflowContext, WorkflowEvent};

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
                report_path: PathBuf::from("runs/truonglao-mytestdl1.md"),
                verdict: None,
                findings_count: None,
                is_challenge: None,
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
                report_path: PathBuf::from("runs/truonglao-failtestdl1.md"),
                verdict: None,
                findings_count: None,
                is_challenge: None,
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
            quorum: Some(1),
            role: None,
            chair_owner: None,
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
            quorum: Some(2),
            role: None,
            chair_owner: None,
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
            members: vec![m1.clone()],
            quorum: Some(1),
            role: None,
            chair_owner: None,
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

        // 4. Council with 0 or >5 members fails
        let input_council_0 = TaskforceInput {
            mode: "council".to_string(),
            task: "t4".to_string(),
            repo: "repo".to_string(),
            brief: "brief".to_string(),
            members: vec![],
            quorum: Some(1),
            role: None,
            chair_owner: None,
        };
        let mut state_council_0 = TaskforceState::from_input(&input_council_0);
        let dec4 = workflow.step(&mut state_council_0, &mut ctx, None);
        match dec4 {
            StepDecision::Fail { code, reason } => {
                assert_eq!(code, 1);
                assert!(reason.contains("council mode requires between 1 and 5 members"));
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
            verdict: None,
            findings_count: None,
            is_challenge: None,
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

    #[test]
    fn test_resolve_council_members_from_taskforce_config() {
        let mut cfg = crate::config::Config::default();
        cfg.taskforce.insert(
            "council".to_string(),
            crate::config::Taskforce {
                members: vec!["truonglao".to_string(), "suhuynh".to_string()],
                read_only: true,
            },
        );

        let (members, quorum) = resolve_council_members(&cfg, None, "t1");
        assert_eq!(
            members,
            vec![
                ("truonglao-t1c1".to_string(), "truonglao".to_string()),
                ("suhuynh-t1c2".to_string(), "suhuynh".to_string()),
            ]
        );
        assert_eq!(quorum, 2);
    }

    #[test]
    fn test_resolve_council_members_fallback_when_taskforce_absent() {
        let mut cfg = crate::config::Config::default();
        cfg.purposes
            .insert("review".to_string(), "suhuynh".to_string());
        cfg.purposes
            .insert("design".to_string(), "truonglao".to_string());

        let (members, quorum) = resolve_council_members(&cfg, None, "t2");
        assert_eq!(
            members,
            vec![
                ("suhuynh-t2c1".to_string(), "suhuynh".to_string()),
                ("truonglao-t2c2".to_string(), "truonglao".to_string()),
            ]
        );
        assert_eq!(quorum, 2);
    }

    #[test]
    fn test_resolve_council_members_fallback_picks_three_distinct_when_available() {
        let mut cfg = crate::config::Config::default();
        cfg.purposes
            .insert("review".to_string(), "suhuynh".to_string());
        cfg.purposes
            .insert("design".to_string(), "truonglao".to_string());
        cfg.strategy.default_reviewer.push("sude".to_string());

        let (members, quorum) = resolve_council_members(&cfg, None, "t3");
        assert_eq!(
            members,
            vec![
                ("suhuynh-t3c1".to_string(), "suhuynh".to_string()),
                ("truonglao-t3c2".to_string(), "truonglao".to_string()),
                ("sude-t3c3".to_string(), "sude".to_string()),
            ]
        );
        assert_eq!(quorum, 2);
    }

    #[test]
    fn council_neutral_seat() {
        use std::path::Path;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};

        let tmp = std::env::temp_dir().join(format!(
            "quaqua-cns-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let runs_dir = tmp.join("runs");
        let repo_dir = tmp.join("repo");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&repo_dir).unwrap();

        struct NeutralSeatMock {
            runs_dir: PathBuf,
            prompts: Mutex<std::collections::HashMap<String, String>>,
        }

        impl CouncilMemberRunner for NeutralSeatMock {
            fn spawn_member(
                &self,
                _name: &str,
                _kind: &str,
                _repo: &Path,
                _task: &str,
                _approve_kinds: &[String],
                _base: Option<&str>,
            ) -> Result<(), (i32, String)> {
                Ok(())
            }

            fn prompt_member(
                &self,
                name: &str,
                brief: &str,
                _repo: &Path,
                _cancel: Arc<AtomicBool>,
            ) -> Result<SingleWorkerOutput, (i32, String)> {
                self.prompts
                    .lock()
                    .unwrap()
                    .insert(name.to_string(), brief.to_string());

                let sealed_dir = self.runs_dir.join(".sealed");
                std::fs::create_dir_all(&sealed_dir).unwrap();
                let sealed_report = sealed_dir.join(format!("{name}.md"));
                let unsealed_report = self.runs_dir.join(format!("{name}.md"));

                // Write member report to runs/.sealed/<name>.md
                std::fs::write(&sealed_report, "# Report\nVerdict: clean\n").unwrap();

                // (2) Sealed member report is NOT readable under runs/<name>.md before round closes
                assert!(
                    !unsealed_report.exists(),
                    "sealed member report must NOT be readable under runs/<name>.md before round closes"
                );
                assert!(
                    sealed_report.exists(),
                    "sealed report must exist in runs/.sealed/<name>.md while round is open"
                );

                Ok(SingleWorkerOutput::new(0, sealed_report).unwrap())
            }

            fn cleanup_member(&self, _name: &str, _repo: &Path) -> Result<(), (i32, String)> {
                Ok(())
            }
        }

        let runner = NeutralSeatMock {
            runs_dir: runs_dir.clone(),
            prompts: Mutex::new(std::collections::HashMap::new()),
        };

        let task = "cnstest";
        let brief = "Goal: audit neutral seat\nHard constraints: none\nCurrent design (open to challenge): use approach X\nEvidence: clean";
        let m1 = format!("suhuynh-{}c1", task);
        let m2 = format!("truonglao-{}c2", task);
        let member_specs = vec![
            (m1.clone(), "suhuynh".to_string()),
            (m2.clone(), "truonglao".to_string()),
        ];

        let out = run_taskforce_workflow(
            "council",
            task,
            &repo_dir,
            brief,
            "council",
            &member_specs,
            2,
            "cos",
            &[],
            None,
            &runs_dir,
            &runner,
        )
        .expect("taskforce workflow should complete successfully");

        // Assertion (1): neutral seat prompt omits Current design while others retain it
        let prompts = runner.prompts.lock().unwrap();
        let prompt_m1 = prompts
            .get(&m1)
            .expect("prompt for neutral seat m1 must be recorded");
        let prompt_m2 = prompts
            .get(&m2)
            .expect("prompt for second seat m2 must be recorded");

        assert!(
            !prompt_m1.contains("Current design"),
            "neutral seat prompt must omit Current design, got: {prompt_m1}"
        );
        assert!(
            prompt_m1.contains("Reconstruct the problem first")
                || prompt_m1.contains("reconstruct the problem first"),
            "neutral seat prompt must instruct to reconstruct the problem first, got: {prompt_m1}"
        );
        assert!(
            prompt_m2.contains("Current design"),
            "non-neutral seat prompt must retain Current design, got: {prompt_m2}"
        );
        drop(prompts);

        // Assertion (2): sealed member report revealed into runs/<name>.md upon round completion
        let unsealed_m1 = runs_dir.join(format!("{m1}.md"));
        let unsealed_m2 = runs_dir.join(format!("{m2}.md"));
        let sealed_m1 = runs_dir.join(".sealed").join(format!("{m1}.md"));
        let sealed_m2 = runs_dir.join(".sealed").join(format!("{m2}.md"));

        assert!(
            unsealed_m1.exists(),
            "unsealed m1 report must exist after round closes"
        );
        assert!(
            unsealed_m2.exists(),
            "unsealed m2 report must exist after round closes"
        );
        assert!(
            !sealed_m1.exists(),
            "sealed m1 file must be moved into runs/ after round closes"
        );
        assert!(
            !sealed_m2.exists(),
            "sealed m2 file must be moved into runs/ after round closes"
        );
        assert_eq!(
            out.report_paths,
            vec![unsealed_m1.clone(), unsealed_m2.clone()]
        );

        let content_before = std::fs::read_to_string(&unsealed_m1).unwrap();

        // Assertion (3): replaying journal does not double-move
        let replay_out = run_taskforce_workflow(
            "council",
            task,
            &repo_dir,
            brief,
            "council",
            &member_specs,
            2,
            "cos",
            &[],
            None,
            &runs_dir,
            &runner,
        )
        .expect("journal replay must succeed");

        assert_eq!(
            replay_out.report_paths,
            vec![unsealed_m1.clone(), unsealed_m2.clone()],
            "replay must preserve report paths pointing to runs/<name>.md"
        );
        assert!(
            unsealed_m1.exists(),
            "unsealed m1 report must still exist after journal replay"
        );
        assert!(
            !sealed_m1.exists(),
            "sealed m1 report must not be re-created on journal replay"
        );
        let content_after = std::fs::read_to_string(&unsealed_m1).unwrap();
        assert_eq!(
            content_before, content_after,
            "report content must not be modified or corrupted by replay"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
