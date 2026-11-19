//! Caller-aware tier verdict (R7): the strictest of the repo tier, the plan
//! lane and the action class decides what the CoS may do alone.

use std::path::{Path, PathBuf};

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
    /// `quaqua call grant.*`: anh iu's consent, asked at every tier (R14).
    GrantWrite,
    /// USER.md and memory/rules.md: the CoS's alone, no per-write ask (R4).
    MemoryFile,
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
        | InstructionFile | GrantWrite | MemoryFile => None,
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
        Floor | TrunkMove | Bypass | DbWrite | InstructionFile | GrantWrite => Decision::Ask,
        PaneInput | MemoryFile => Decision::Allow,
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

/// The key a repo is stored under: the main checkout of the git repo that holds
/// `path` (a linked worktree maps to its main repo), else the canonical path.
/// The grant verbs and the gate both use it, so a write and a read meet.
pub fn repo_key(path: &Path) -> String {
    let dir = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let common = std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    let root = match common {
        Some(c) if c.file_name().is_some_and(|n| n == ".git") => {
            c.parent().map_or(dir, Path::to_path_buf)
        }
        Some(c) => c,
        None => dir,
    };
    root.to_string_lossy().into_owned()
}

fn parse_tier(s: &str) -> Option<Tier> {
    match s {
        "low" => Some(Tier::Low),
        "medium" => Some(Tier::Medium),
        "high" => Some(Tier::High),
        _ => None,
    }
}

pub fn tier_word(t: Tier) -> &'static str {
    match t {
        Tier::Low => "low",
        Tier::Medium => "medium",
        Tier::High => "high",
    }
}

/// One tier from a `(key, tier)` table, read-only: a missing database, table,
/// row or unreadable word is high (Q15), and a read never creates the file.
fn stored_tier(runs_dir: &Path, table: &str, col: &str, key: &str) -> Tier {
    let db = runs_dir.join("qq.db");
    if !db.is_file() {
        return Tier::High;
    }
    let read = || -> rusqlite::Result<Option<String>> {
        let conn =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut st = conn.prepare(&format!("SELECT tier FROM {table} WHERE {col} = ?1"))?;
        let mut rows = st.query([key])?;
        rows.next()?.map(|r| r.get(0)).transpose()
    };
    read()
        .ok()
        .flatten()
        .and_then(|w| parse_tier(&w))
        .unwrap_or(Tier::High)
}

/// The repo's tier from the daemon-owned `repo_tier` row (R14); an unlisted
/// repo is high.
pub fn repo_tier(runs_dir: &Path, repo: &Path) -> Tier {
    stored_tier(runs_dir, "repo_tier", "repo", &repo_key(repo))
}

/// The tier of plan lane `lane` from the daemon-owned `plan_lane` row; an
/// unknown lane is high.
pub fn lane_tier_of(runs_dir: &Path, lane: &str) -> Tier {
    stored_tier(runs_dir, "plan_lane", "lane", lane)
}

/// The default lane tier: high. No production caller reads it any more (the
/// gate and `pr.merge` use [`lane_tier_of`]); `tests/authority.rs` still
/// asserts it, so it stays until that test moves.
pub fn lane_tier() -> Tier {
    Tier::High
}

/// True when the daemon-owned `grant_full` table holds a row for `repo`'s key
/// (R14/Q17). Read-only: a missing database, table or row is no grant.
pub fn grant_full(runs_dir: &Path, repo: &Path) -> bool {
    let db = runs_dir.join("qq.db");
    if !db.is_file() {
        return false;
    }
    let key = repo_key(repo);
    let read = || -> rusqlite::Result<bool> {
        let conn =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.query_row(
            "SELECT 1 FROM grant_full WHERE repo = ?1",
            [key],
            |_| Ok(()),
        )
        .map(|()| true)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(false),
            e => Err(e),
        })
    };
    read().unwrap_or(false)
}

/// A full grant for `repo` lowers `base` to low for the CoS only; any other
/// caller, or no grant, keeps `base`. The floor, push to main and the fence
/// paths never read this: `decide` and `pr_tier` keep them.
pub fn apply_grant(runs_dir: &Path, repo: &Path, caller_is_cos: bool, base: Tier) -> Tier {
    if caller_is_cos && grant_full(runs_dir, repo) {
        Tier::Low
    } else {
        base
    }
}

/// The tier the gate and `pr.merge` judge an action in: the strictest of the
/// repo's `repo_tier` row and its `plan_lane` row (keyed by the repo key; an
/// unknown lane is high), lowered to low by a full grant for the CoS.
pub fn effective_context_tier(runs_dir: &Path, dir: &Path, caller_is_cos: bool) -> Tier {
    let base = repo_tier(runs_dir, dir).max(lane_tier_of(runs_dir, &repo_key(dir)));
    apply_grant(runs_dir, dir, caller_is_cos, base)
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

/// A local branch already merged into the remote default is a low-tier delete;
/// any other (unmerged, unknown, git failing) is medium.
pub fn branch_delete_action(cwd: &Path, branch: &str) -> Action {
    branch_delete_class(cwd, branch, None)
}

/// Class of a delete of `branch`, judged on the ref the delete removes: the
/// remote-tracking ref `refs/remotes/<remote>/<branch>` for a push delete, else
/// the local branch. Merged means `git merge-base --is-ancestor <tip> <remote
/// default>`; a missing ref, a missing default or git failing is unmerged.
pub fn branch_delete_class(cwd: &Path, branch: &str, remote: Option<&str>) -> Action {
    let unmerged = Action::BranchDeleteUnmerged;
    if branch.is_empty() || branch.starts_with('-') || remote.is_some_and(|r| r.starts_with('-')) {
        return unmerged;
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
        return unmerged;
    }
    let tip = match remote {
        Some(r) => format!("refs/remotes/{r}/{branch}"),
        None => format!("refs/heads/{branch}"),
    };
    if git_ok(cwd, &["rev-parse", "--verify", "--quiet", &tip])
        && git_ok(cwd, &["merge-base", "--is-ancestor", &tip, &default])
    {
        Action::BranchDeleteMerged
    } else {
        unmerged
    }
}
