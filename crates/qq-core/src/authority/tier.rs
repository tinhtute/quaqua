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

pub fn action_tier(_a: Action) -> Option<Tier> {
    None
}

pub fn decide(_cos: bool, _a: Action, _repo: Tier, _lane: Tier) -> Decision {
    Decision::Deny
}

pub fn repo_tier(_runs_dir: &Path, _repo: &Path) -> Tier {
    Tier::High
}

pub fn lane_tier() -> Tier {
    Tier::High
}

pub fn branch_delete_action(_cwd: &Path, _branch: &str) -> Action {
    Action::BranchDeleteUnmerged
}
