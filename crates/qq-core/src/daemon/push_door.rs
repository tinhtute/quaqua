//! `push.cos` (R10): stub, red phase.

use std::path::Path;

use serde_json::Value;

use super::Daemon;

pub const VERB: &str = "push.cos";

pub fn push(_runs_dir: &Path, _repo: &Path, _remote: &str, _branch: &str) -> Result<Value, String> {
    Err("stub".into())
}

pub fn register(_d: &mut Daemon) {}
