//! `pr.merge` (R10, R13): the one door to merge a PR into main. Stub until the
//! green commit; the tests in `tests/orchestration_verbs.rs` pin the contract.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::Daemon;
use crate::authority::tier::Tier;

pub const VERB: &str = "pr.merge";

/// True when `path` (repo-relative) sits under a fence path (R13).
pub fn fence_path(_path: &str) -> bool {
    false
}

/// The tier of a PR whose diff touches `files`.
pub fn pr_tier(repo: Tier, lane: Tier, _files: &[String]) -> Tier {
    repo.max(lane)
}

/// Test seam: the gh program and the base tiers for one workspace.
#[doc(hidden)]
pub fn set_test_hooks(_runs_dir: &Path, _gh: PathBuf, _repo: Tier, _lane: Tier) {}

pub fn register(d: &mut Daemon) {
    let _ = d.register_verb(VERB, Box::new(|_, _| Ok(json!({ "stub": true }))));
}
