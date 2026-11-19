//! `pr.merge` (R10, R13): the one door to merge a PR into main.
//!
//! The daemon runs `gh pr merge --match-head-commit <sha>` only when all of
//! these hold, each read from the daemon's own state, never from the request:
//! the caller resolves to `cos` from the socket peer; the daemon-computed tier
//! allows a merge request; the sha is the one the workflow journal says passed
//! review; the PR head is that sha; and `gh pr checks` is green. The request
//! names only a workflow id, a PR number and a sha, each validated before it
//! reaches an argv. `gh` always runs as an argv, never through a shell.
//!
//! GitHub branch protection on main is the fence behind this door; a refusal
//! here is the first line, not the only one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use super::{caller, Ctx, Daemon};
use crate::authority::tier::{self, Action, Decision, Tier};
use crate::engine::journal::Journal;
use crate::rpc::{Request, RpcError};

pub const VERB: &str = "pr.merge";

const REFUSED: &str = "refused";

/// Paths whose change makes a PR tier high whatever the grant (R13).
const FENCE_PREFIXES: &[&str] = &[
    "crates/qq-core/src/authority/",
    "crates/qq-core/src/identity/",
    "crates/qq-core/src/merge/",
    "crates/qq-core/src/daemon/",
    "crates/qq-core/templates/workspace/scripts/hooks/",
    "crates/qq-core/templates/workspace/.claude/",
];
const FENCE_FILES: &[&str] = &[
    ".claude/settings.json",
    "crates/qq-core/templates/workspace/AGENTS.md",
];

/// True when `path` (repo-relative, as `gh pr diff --name-only` prints it)
/// sits under a fence path.
pub fn fence_path(path: &str) -> bool {
    // git quotes a path with unusual bytes; the fence names are plain ASCII, so
    // a quote only wraps the tail and never hides a fence prefix.
    let p = path.trim().trim_start_matches('"');
    let p = p.strip_prefix("./").unwrap_or(p);
    FENCE_FILES.contains(&p) || FENCE_PREFIXES.iter().any(|f| p.starts_with(f))
}

/// The tier of a PR whose diff touches `files`: the repo and lane tiers, and
/// high when any file is a fence path.
pub fn pr_tier(repo: Tier, lane: Tier, files: &[String]) -> Tier {
    let base = repo.max(lane);
    if files.iter().any(|f| fence_path(f)) {
        Tier::High
    } else {
        base
    }
}

/// Every path a unified diff names in its headers: both sides of a rename or
/// copy, the old and new side of each file. Over-matching only fails closed.
pub fn diff_paths(diff: &str) -> Vec<String> {
    const HEADERS: &[&str] = &[
        "diff --git ",
        "rename from ",
        "rename to ",
        "copy from ",
        "copy to ",
        "--- ",
        "+++ ",
    ];
    let mut out = Vec::new();
    for line in diff.lines() {
        let Some(rest) = HEADERS.iter().find_map(|h| line.strip_prefix(h)) else {
            continue;
        };
        out.push(rest.to_string());
        for tok in rest.split_whitespace() {
            let t = tok.trim_matches('"');
            out.push(
                t.strip_prefix("a/")
                    .or(t.strip_prefix("b/"))
                    .unwrap_or(t)
                    .to_string(),
            );
        }
    }
    out
}

struct Hooks {
    gh: PathBuf,
    repo: Tier,
    lane: Tier,
}

fn hooks() -> &'static Mutex<HashMap<PathBuf, Hooks>> {
    static H: OnceLock<Mutex<HashMap<PathBuf, Hooks>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Test seam: the gh program and the base tiers for the workspace at
/// `runs_dir`. Process-local, keyed by workspace, never reachable by a request;
/// production sets nothing and runs `gh` with the tiers `authority::tier` gives.
#[doc(hidden)]
pub fn set_test_hooks(runs_dir: &Path, gh: PathBuf, repo: Tier, lane: Tier) {
    hooks()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(runs_dir.to_path_buf(), Hooks { gh, repo, lane });
}

fn refused(why: impl Into<String>) -> RpcError {
    RpcError::new(REFUSED, why.into())
}

fn bad(why: impl Into<String>) -> RpcError {
    RpcError::new(crate::rpc::BAD_REQUEST, why.into())
}

/// A refusal, journaled like every other authority refusal.
fn deny(ctx: &Ctx, req: &Request, target: &str, why: &str) -> RpcError {
    let who = caller::identified(req).unwrap_or("unknown");
    let _ = ctx
        .store
        .with_conn(|c| crate::authority::refuse(c, who, VERB, target, why));
    refused(format!("{who} -> {target}: {why}"))
}

fn gh(program: &Path, repo: &str, args: &[&str]) -> std::io::Result<Output> {
    Command::new(program)
        .args(args)
        .current_dir(repo)
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .output()
}

fn ok_stdout(o: std::io::Result<Output>) -> Option<String> {
    let o = o.ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// What the workflow journal says about a review: `(repo, reviewed sha)` when
/// the latest snapshot holds a clean review of a sha that is at or past the
/// gate (a sha still under review is not yet reviewed).
fn reviewed(ctx: &Ctx, workflow: &str) -> Option<(String, String)> {
    let rows = ctx.store.workflows().ok()?;
    if !rows
        .iter()
        .any(|w| w.id == workflow && w.kind == crate::task::pipeline::KIND)
    {
        return None;
    }
    let snap = ctx.store.journal().load_latest_snapshot(workflow).ok()??;
    let v: Value = serde_json::from_str(&snap.state_json).ok()?;
    let at_gate = match v.get("stage").and_then(Value::as_str)? {
        "gate" | "merge" => true,
        "blocked" => v.pointer("/blocked/kind").and_then(Value::as_str) == Some("hold"),
        _ => false,
    };
    if !at_gate || v.get("findings").and_then(Value::as_u64) != Some(0) {
        return None;
    }
    let sha = v.get("reviewed").and_then(Value::as_str)?.to_string();
    let repo = v
        .pointer("/input/repo")
        .and_then(Value::as_str)?
        .to_string();
    (!repo.is_empty() && crate::merge::is_full_sha(&sha)).then_some((repo, sha))
}

fn checks_green(program: &Path, repo: &str, pr: &str) -> bool {
    let Some(out) = ok_stdout(gh(program, repo, &["pr", "checks", pr, "--json", "bucket"])) else {
        return false;
    };
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&out) else {
        return false;
    };
    !rows.is_empty()
        && rows.iter().all(|r| {
            matches!(
                r.get("bucket").and_then(Value::as_str),
                Some("pass" | "skipping")
            )
        })
}

fn handle(ctx: &Ctx, req: &Request) -> Result<Value, RpcError> {
    if !caller::is_cos(req) {
        return Err(deny(ctx, req, "pr.merge", "only the CoS merges to main"));
    }
    let arg = |k: &str| req.args.get(k).and_then(Value::as_str);
    let workflow = arg("workflow")
        .filter(|w| !w.is_empty())
        .ok_or_else(|| bad("missing string argument 'workflow'"))?;
    let pr = match req.args.get("pr") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err(bad("missing argument 'pr'")),
    };
    if pr.is_empty() || pr.len() > 9 || !pr.bytes().all(|b| b.is_ascii_digit()) {
        return Err(deny(ctx, req, "pr.merge", "pr must be a PR number"));
    }
    let sha = arg("sha")
        .filter(|s| crate::merge::is_full_sha(s))
        .ok_or_else(|| deny(ctx, req, &pr, "sha must be a full 40-hex sha"))?
        .to_ascii_lowercase();
    let target = format!("pr {pr} @ {sha}");

    let Some((repo, journaled)) = reviewed(ctx, workflow) else {
        return Err(deny(
            ctx,
            req,
            &target,
            "workflow has no clean reviewed sha",
        ));
    };
    if !journaled.eq_ignore_ascii_case(&sha) {
        return Err(deny(ctx, req, &target, "sha is not the reviewed sha"));
    }

    let (program, base_repo, base_lane) = {
        let h = hooks().lock().unwrap_or_else(|p| p.into_inner());
        match h.get(&ctx.runs_dir) {
            Some(h) => (h.gh.clone(), h.repo, h.lane),
            None => (
                PathBuf::from("gh"),
                tier::repo_tier(&ctx.runs_dir, Path::new(&repo)),
                tier::lane_tier(),
            ),
        }
    };
    // A high base needs no diff to refuse.
    if base_repo.max(base_lane) >= Tier::High {
        return Err(deny(
            ctx,
            req,
            &target,
            "tier High does not allow the CoS to merge alone",
        ));
    }
    let head = ok_stdout(gh(
        &program,
        &repo,
        &[
            "pr",
            "view",
            &pr,
            "--json",
            "headRefOid",
            "--jq",
            ".headRefOid",
        ],
    ));
    if !head.is_some_and(|h| h.eq_ignore_ascii_case(&sha)) {
        return Err(deny(ctx, req, &target, "PR head is not the reviewed sha"));
    }
    // The whole diff, not `--name-only`: the old side of a rename is a path too.
    // An unreadable diff is high.
    let tier = match ok_stdout(gh(
        &program,
        &repo,
        &["pr", "diff", &pr, "--color", "never"],
    )) {
        Some(out) => pr_tier(base_repo, base_lane, &diff_paths(&out)),
        None => Tier::High,
    };
    if tier::decide(true, Action::MergeRequest, tier, base_lane) != Decision::Allow {
        return Err(deny(
            ctx,
            req,
            &target,
            &format!("tier {tier:?} does not allow the CoS to merge alone"),
        ));
    }
    if !checks_green(&program, &repo, &pr) {
        return Err(deny(ctx, req, &target, "checks are not green on the sha"));
    }

    let out = gh(
        &program,
        &repo,
        &["pr", "merge", &pr, "--match-head-commit", &sha, "--merge"],
    )
    .map_err(|e| RpcError::new("merge_failed", e.to_string()))?;
    if !out.status.success() {
        return Err(RpcError::new(
            "merge_failed",
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(json!({ "merged": true, "pr": pr, "sha": sha }))
}

pub fn register(d: &mut Daemon) {
    let _ = d.register_verb(VERB, Box::new(handle));
}
