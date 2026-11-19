//! Caller-aware tier verdict (R7): the strictest of the repo tier, the plan
//! lane and the action class decides what the CoS may do alone.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PushNonMain,
    OpenPr,
    BranchDeleteMerged,
    BranchDeleteUnmerged,
    MergeRequest,
    Tag,
    Release,
    DeleteData,
    HistoryRewrite,
    Floor,
    PushMain,
    ForcePush,
    TrunkMove,
    Bypass,
    DbWrite,
    PaneInput,
    InstructionFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

/// The tier an action class carries by itself. `None` for the actions that
/// bypass the tier math (floor, always-deny, guards).
pub fn action_tier(a: Action) -> Option<Tier> {
    use Action::*;
    match a {
        PushNonMain | OpenPr | BranchDeleteMerged => Some(Tier::Low),
        BranchDeleteUnmerged | MergeRequest | Tag | Release | DeleteData => Some(Tier::Medium),
        HistoryRewrite => Some(Tier::High),
        Floor | PushMain | ForcePush | TrunkMove | Bypass | DbWrite | PaneInput
        | InstructionFile => None,
    }
}

/// The verdict for `action` (R7). Only the CoS gets past `Deny`. For the CoS:
/// pushing to main and force-pushing deny always; the floor (spend, deploy,
/// send outward) and the guard actions ask at every tier; pane input is the
/// CoS's to send; the rest follow the strictest of repo tier, plan lane and
/// action class: low and medium allow, high allows only a non-main push and
/// opening a PR and asks for the rest.
pub fn decide(cos: bool, action: Action, repo: Tier, lane: Tier) -> Decision {
    use Action::*;
    if !cos {
        return Decision::Deny;
    }
    match action {
        PushMain | ForcePush => Decision::Deny,
        Floor | TrunkMove | Bypass | DbWrite | InstructionFile => Decision::Ask,
        PaneInput => Decision::Allow,
        _ => {
            let tier = repo
                .max(lane)
                .max(action_tier(action).unwrap_or(Tier::High));
            if tier <= Tier::Medium || matches!(action, PushNonMain | OpenPr) {
                Decision::Allow
            } else {
                Decision::Ask
            }
        }
    }
}

/// The repo's tier from the daemon-owned row. The writer and table arrive with
/// the grant verb (R14); until then every repo is unlisted, so high.
pub fn repo_tier(_runs_dir: &Path, _repo: &Path) -> Tier {
    Tier::High
}

/// The plan lane's tier. The gate has no way to see the lane yet, so the
/// default high-risk lane applies.
pub fn lane_tier() -> Tier {
    Tier::High
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A branch already merged into the remote default is a low-tier delete; any
/// other (unmerged, unknown, git failing) is medium. `git merge-base
/// --is-ancestor <branch> <remote default>`.
pub fn branch_delete_action(cwd: &Path, branch: &str) -> Action {
    if branch.is_empty() || branch.starts_with('-') {
        return Action::BranchDeleteUnmerged;
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let default = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => ["origin/main", "origin/master"]
            .into_iter()
            .find(|r| git_ok(cwd, &["rev-parse", "--verify", "--quiet", r]))
            .unwrap_or_default()
            .to_string(),
    };
    if default.is_empty() {
        return Action::BranchDeleteUnmerged;
    }
    let tip = [
        format!("refs/heads/{branch}"),
        format!("refs/remotes/origin/{branch}"),
    ]
    .into_iter()
    .find(|r| git_ok(cwd, &["rev-parse", "--verify", "--quiet", r]));
    match tip {
        Some(t) if git_ok(cwd, &["merge-base", "--is-ancestor", &t, &default]) => {
            Action::BranchDeleteMerged
        }
        _ => Action::BranchDeleteUnmerged,
    }
}
