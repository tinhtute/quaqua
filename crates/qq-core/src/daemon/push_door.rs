//! `push.cos` (R10, Q16, R7): the one door for the CoS to push a non-main
//! branch. The caller must resolve to `cos` from the socket peer (never from
//! the request); `main`, `master`, the remote default branch and any forcing
//! form stay refused; the push is classified with `authority::tier`
//! (`PushNonMain`, judged in the repo's effective context tier) and runs as
//! a plain `git push <remote> refs/heads/<b>:refs/heads/<b>` argv, no force.
//! The fenced-Lead `push` verb is unchanged.

use std::path::Path;

use serde_json::{json, Value};

use super::{caller, Ctx, Daemon};
use crate::authority::push::{configured_remote, git, plain_branch, protected_branch};
use crate::authority::tier::{self, Action, Decision};
use crate::rpc::{self, Request, RpcError};

pub const VERB: &str = "push.cos";

fn refused(ctx: &Ctx, req: &Request, target: &str, why: &str) -> RpcError {
    let who = caller::identified(req).unwrap_or("unknown");
    let _ = ctx
        .store
        .with_conn(|c| crate::authority::refuse(c, who, VERB, target, why));
    RpcError::new("refused", format!("{who} -> {target}: {why}"))
}

fn handle(ctx: &Ctx, req: &Request) -> Result<Value, RpcError> {
    if !caller::is_cos(req) {
        return Err(refused(
            ctx,
            req,
            VERB,
            "only the CoS pushes through this door",
        ));
    }
    let arg = |k: &str| {
        req.args
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| RpcError::new(rpc::BAD_REQUEST, format!("missing argument '{k}'")))
    };
    let (repo, remote, branch) = (arg("repo")?, arg("remote")?, arg("branch")?);
    let repo_dir = std::fs::canonicalize(repo)
        .map_err(|_| refused(ctx, req, repo, "repo is not a directory"))?;
    let repo = repo_dir.to_string_lossy().into_owned();
    let repo = repo.as_str();

    // Any force flag is a ForcePush, which the tier table denies always.
    let forced = req
        .args
        .get("force")
        .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)));
    if forced
        && tier::decide(true, Action::ForcePush, tier::Tier::Low, tier::Tier::Low)
            != Decision::Allow
    {
        return Err(refused(ctx, req, branch, "force-push is never allowed"));
    }
    if !plain_branch(repo, branch) {
        return Err(refused(
            ctx,
            req,
            branch,
            "branch must be a plain branch name",
        ));
    }
    if !configured_remote(repo, remote) {
        return Err(refused(
            ctx,
            req,
            remote,
            "remote is not configured in the repo",
        ));
    }
    if protected_branch(repo, remote, branch)
        && tier::decide(true, Action::PushMain, tier::Tier::Low, tier::Tier::Low) != Decision::Allow
    {
        return Err(refused(
            ctx,
            req,
            branch,
            "branch is the repo's main or default branch",
        ));
    }
    let base = tier::effective_context_tier(&ctx.runs_dir, &repo_dir, true);
    if tier::decide(true, Action::PushNonMain, base, base) != Decision::Allow {
        return Err(refused(
            ctx,
            req,
            branch,
            &format!("tier {base:?} does not allow the CoS to push alone"),
        ));
    }
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    let out = git(repo)
        .args(["push", "--", remote, &refspec])
        .output()
        .map_err(|e| RpcError::new("push_failed", e.to_string()))?;
    if !out.status.success() {
        return Err(RpcError::new(
            "push_failed",
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(json!({ "pushed": true, "remote": remote, "branch": branch }))
}

/// Client side: asks the daemon of `runs_dir`'s workspace to push. A daemon
/// that is down, or a reply that is not a success, is an `Err` (fail closed).
pub fn push(runs_dir: &Path, repo: &Path, remote: &str, branch: &str) -> Result<Value, String> {
    let socket = rpc::socket_path(runs_dir);
    let req = Request::new(
        VERB,
        json!({ "repo": repo, "remote": remote, "branch": branch }),
    );
    let resp = rpc::call(&socket, &req).map_err(|e| format!("no daemon answer: {e}"))?;
    resp.into_result()
        .map_err(|e| format!("{}: {}", e.code, e.message))
}

pub fn register(d: &mut Daemon) {
    let _ = d.register_verb(VERB, Box::new(handle));
}
