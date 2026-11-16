//! The merge queue (ported from `crates/quaqua/src/merge`): one branch at a
//! time into the project integration branch, a check after each merge, undo
//! on failure, never a push.
//!
//! Changes against the port: the queue takes its inputs as data (no team.toml,
//! no env, no verdict table), and every entry may carry the sha the review
//! was bound to. An entry whose branch tip is not that sha is refused before
//! anything is merged, and the merge takes the reviewed sha, not the branch
//! name, so a commit that lands after the check of the tip cannot slip in.
//! A repeat converges: an entry already in the integration branch is skipped.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{Daemon, Handler};
use crate::rpc::{self, RpcError};

/// The queue is one lane: concurrent callers wait for their turn.
static LANE: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub branch: String,
    /// The reviewed candidate; `None` merges whatever the tip is.
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub repo: PathBuf,
    pub integ: String,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub check: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merged {
    pub branch: String,
    /// The integration head after this entry.
    pub head: String,
    /// False when the entry was already in the integration branch.
    pub merged_now: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeError {
    /// Bad input, a missing branch or a dirty worktree (old exit 1).
    Operational(String),
    /// The check failed with the branch merged (old exit 2); the merge is undone.
    CheckFailed { branch: String },
    /// The check fails on the integration base too (old exit 3): hold the queue.
    BaseBroken { branch: String },
    /// A merge conflict (old exit 4); the merge is aborted.
    Conflict { branch: String },
    /// The branch tip is not the reviewed sha (R22); nothing was merged for it.
    ShaMoved {
        branch: String,
        reviewed: String,
        tip: String,
    },
}

impl MergeError {
    pub fn code(&self) -> &'static str {
        match self {
            MergeError::Operational(_) => "merge_failed",
            MergeError::CheckFailed { .. } => "check_failed",
            MergeError::BaseBroken { .. } => "base_broken",
            MergeError::Conflict { .. } => "merge_conflict",
            MergeError::ShaMoved { .. } => "sha_moved",
        }
    }
}

impl std::fmt::Display for MergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MergeError::Operational(m) => write!(f, "{m}"),
            MergeError::CheckFailed { branch } => {
                write!(f, "check failed with '{branch}' merged; merge undone")
            }
            MergeError::BaseBroken { branch } => write!(
                f,
                "check fails on the integration base without '{branch}'; queue held"
            ),
            MergeError::Conflict { branch } => write!(f, "merge conflict merging '{branch}'"),
            MergeError::ShaMoved {
                branch,
                reviewed,
                tip,
            } => write!(
                f,
                "'{branch}' moved after review (reviewed {reviewed}, tip {tip})"
            ),
        }
    }
}

/// A full 40-hex sha, the only form a review or an acceptance may bind to.
pub fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn git(dir: &Path, args: &[&str]) -> std::io::Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "qq")
        .env("GIT_AUTHOR_EMAIL", "qq@local")
        .env("GIT_COMMITTER_NAME", "qq")
        .env("GIT_COMMITTER_EMAIL", "qq@local")
        .output()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// The commit `rev` names in `dir`, or `None`.
pub fn resolve(dir: &Path, rev: &str) -> Option<String> {
    let spec = format!("{rev}^{{commit}}");
    git(dir, &["rev-parse", "--verify", "--quiet", &spec])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| stdout(&o))
        .filter(|s| is_full_sha(s))
}

fn find_worktree_for(repo: &Path, branch: &str) -> Option<PathBuf> {
    let o = git(repo, &["worktree", "list", "--porcelain"]).ok()?;
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let want = format!("branch refs/heads/{branch}");
    let mut cur: Option<PathBuf> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            cur = Some(PathBuf::from(p));
        } else if line == want {
            return cur;
        }
    }
    None
}

/// A temporary worktree on the integration branch, removed on drop.
struct TempWorktree<'a> {
    repo: &'a Path,
    dir: PathBuf,
}

impl Drop for TempWorktree<'_> {
    fn drop(&mut self) {
        let _ = git(
            self.repo,
            &["worktree", "remove", "--force", &self.dir.to_string_lossy()],
        );
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A check passes when it exits 0 and leaves tracked files untouched.
fn run_check(dir: &Path, cmd: Option<&str>) -> bool {
    let Some(cmd) = cmd else { return true };
    let ok = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok && git(dir, &["status", "--porcelain", "--untracked-files=no"])
        .map(|o| o.status.success() && o.stdout.is_empty())
        .unwrap_or(false)
}

fn op(msg: impl Into<String>) -> MergeError {
    MergeError::Operational(msg.into())
}

/// Merges the entries in order. The first failure stops the queue; entries
/// merged before it stay merged.
pub fn run(req: &Request) -> Result<Vec<Merged>, MergeError> {
    let _lane = LANE.lock().unwrap_or_else(|p| p.into_inner());
    let repo = req.repo.as_path();
    let integ_ref = format!("refs/heads/{}", req.integ);
    if resolve(repo, &integ_ref).is_none() {
        return Err(op(format!(
            "integration branch '{}' does not exist in {}",
            req.integ,
            repo.display()
        )));
    }
    // R22 first: refuse a moved entry before the integration branch is touched.
    let mut plan = Vec::new();
    for e in &req.entries {
        let tip = resolve(repo, &format!("refs/heads/{}", e.branch))
            .ok_or_else(|| op(format!("branch '{}' does not exist", e.branch)))?;
        if let Some(sha) = &e.sha {
            if !is_full_sha(sha) {
                return Err(op(format!("'{sha}' is not a full 40-hex sha")));
            }
            if !sha.eq_ignore_ascii_case(&tip) {
                return Err(MergeError::ShaMoved {
                    branch: e.branch.clone(),
                    reviewed: sha.clone(),
                    tip,
                });
            }
        }
        plan.push((e.branch.clone(), tip));
    }

    let (target, _guard) = match find_worktree_for(repo, &req.integ) {
        Some(wt) => (wt, None),
        None => {
            let dir = std::env::temp_dir().join(format!(
                "qq-merge-{}-{}",
                req.integ.replace('/', "_"),
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let added = git(
                repo,
                &["worktree", "add", &dir.to_string_lossy(), &req.integ],
            )
            .map_err(|e| op(format!("git worktree add: {e}")))?;
            if !added.status.success() {
                return Err(op(format!(
                    "cannot open a worktree on '{}': {}",
                    req.integ,
                    String::from_utf8_lossy(&added.stderr).trim()
                )));
            }
            (dir.clone(), Some(TempWorktree { repo, dir }))
        }
    };
    let dirty = git(&target, &["status", "--porcelain"]).map_err(|e| op(e.to_string()))?;
    if !stdout(&dirty).is_empty() {
        return Err(op(format!("worktree {} is dirty", target.display())));
    }

    let mut merged = Vec::new();
    for (branch, tip) in plan {
        let before = resolve(&target, "HEAD").ok_or_else(|| op("cannot read integration HEAD"))?;
        // Already in: a repeat converges here.
        let contained = git(&target, &["merge-base", "--is-ancestor", &tip, "HEAD"])
            .map(|o| o.status.success())
            .unwrap_or(false);
        if contained {
            merged.push(Merged {
                branch,
                head: before,
                merged_now: false,
            });
            continue;
        }
        // The merge takes the sha that was verified, not the moving branch name.
        let out = git(&target, &["merge", "--no-edit", &tip]).map_err(|e| op(e.to_string()))?;
        if !out.status.success() {
            let _ = git(&target, &["merge", "--abort"]);
            return Err(MergeError::Conflict { branch });
        }
        if !run_check(&target, req.check.as_deref()) {
            let reset =
                git(&target, &["reset", "--hard", &before]).map_err(|e| op(e.to_string()))?;
            if !reset.status.success() {
                return Err(op("reset --hard failed while undoing a merge"));
            }
            return Err(if run_check(&target, req.check.as_deref()) {
                MergeError::CheckFailed { branch }
            } else {
                MergeError::BaseBroken { branch }
            });
        }
        let head = resolve(&target, "HEAD").ok_or_else(|| op("cannot read integration HEAD"))?;
        merged.push(Merged {
            branch,
            head,
            merged_now: true,
        });
    }
    Ok(merged)
}

/// The `merge {repo, integ, entries:[{branch, sha?}], [check]}` verb.
pub fn register(daemon: &mut Daemon) -> Result<(), String> {
    let handler: Handler = Box::new(|_, req| {
        let r: Request = serde_json::from_value(req.args.clone())
            .map_err(|e| RpcError::new(rpc::BAD_REQUEST, e.to_string()))?;
        match run(&r) {
            Ok(m) => Ok(json!({ "merged": m })),
            Err(e) => Err(RpcError::new(e.code(), e.to_string())),
        }
    });
    daemon.register_verb("merge", handler)
}

/// JSON form of a request, for the CLI.
pub fn parse_request(text: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    serde_json::from_value::<Request>(v.clone()).map_err(|e| e.to_string())?;
    Ok(v)
}
