//! `quaqua ship <worker>`: push `quaqua/<worker>` and open a PR against the base
//! branch after one explicit Principal approval. There is no merge path: the
//! [`ShipOps`] trait has no merge method, so merging stays manual.

use crate::worker::prompt::{parse_report_outcome, OutcomeLevel};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const USAGE: &str = "\
# Usage: quaqua ship <worker> [--yes] [--repo <dir>] [--base <branch>]
#   Push quaqua/<worker> to origin and open a PR against <branch> (default main).
#   Needs runs/<worker>.md with `Outcome: COMPLETE` and at least one commit ahead of
#   origin/<branch>. Asks y/N on a TTY; without a TTY it refuses unless --yes is given.
#   --yes is the Principal's recorded approval: never pass it on your own. Never merges.
#   Re-running converges: the push is a no-op when current and an open PR is reused.
#
# Exit codes: 0 PR open (created or already there), 1 refused or failed";

/// The PR body always ends with this line.
pub const PR_SIGNATURE: &str = "from therealTINHTUTE with love ❤️❤️❤️";

/// Remote side effects, kept behind a trait so tests never touch git or GitHub.
/// Deliberately has no merge operation.
pub trait ShipOps {
    /// Commits on `branch` that `base_ref` (e.g. `origin/main`) does not have.
    fn commits_ahead(&self, repo: &Path, base_ref: &str, branch: &str) -> Result<u32, String>;
    fn tip_subject(&self, repo: &Path, branch: &str) -> Result<String, String>;
    fn push(&self, repo: &Path, branch: &str) -> Result<(), String>;
    /// URL of an open PR for `branch`, if any.
    fn open_pr(&self, repo: &Path, branch: &str) -> Result<Option<String>, String>;
    fn create_pr(
        &self,
        repo: &Path,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, String>;
}

/// The Principal's yes/no. `summary` is what will happen if approved.
pub trait Approval {
    fn approve(&self, summary: &str) -> bool;
}

/// `--yes`.
pub struct AssumeYes;
impl Approval for AssumeYes {
    fn approve(&self, _summary: &str) -> bool {
        true
    }
}

/// y/N on the terminal; anything but `y`/`yes` (or a read failure) is no.
pub struct TtyPrompt;
impl Approval for TtyPrompt {
    fn approve(&self, summary: &str) -> bool {
        eprint!("{summary}\nPush and open this PR? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

pub struct ShipRequest<'a> {
    pub worker: &'a str,
    pub repo: &'a Path,
    pub base: &'a str,
    pub report: &'a str,
}

/// PR body: the report's header block (up to the first blank line), then the signature.
pub fn pr_body(report: &str) -> String {
    let head: Vec<&str> = report
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .collect();
    format!("{}\n\n{PR_SIGNATURE}\n", head.join("\n"))
}

/// Gate, ask, push, open the PR. Returns the PR URL.
pub fn ship<O: ShipOps, A: Approval>(
    ops: &O,
    approval: &A,
    req: &ShipRequest,
) -> Result<String, (i32, String)> {
    let refuse = |msg: String| Err((1, format!("ship refused: {msg}\n")));
    match parse_report_outcome(req.report) {
        Ok(OutcomeLevel::Complete) => {}
        Ok(other) => return refuse(format!("report Outcome is {other}, not COMPLETE")),
        Err(e) => return refuse(format!("report unreadable: {e}")),
    }
    let branch = format!("quaqua/{}", req.worker);
    let base_ref = format!("origin/{}", req.base);
    let ahead = ops
        .commits_ahead(req.repo, &base_ref, &branch)
        .map_err(|e| (1, format!("ship failed: {e}\n")))?;
    if ahead == 0 {
        return refuse(format!("{branch} has no commits ahead of {base_ref}"));
    }
    let title = ops
        .tip_subject(req.repo, &branch)
        .map_err(|e| (1, format!("ship failed: {e}\n")))?;
    let summary = format!(
        "branch: {branch}\nbase:   {}\nahead:  {ahead} commit(s)\ntitle:  {title}",
        req.base
    );
    if !approval.approve(&summary) {
        return refuse("no Principal approval".to_string());
    }
    let fail = |e: String| (1, format!("ship failed: {e}\n"));
    ops.push(req.repo, &branch).map_err(fail)?;
    if let Some(url) = ops.open_pr(req.repo, &branch).map_err(fail)? {
        return Ok(url);
    }
    ops.create_pr(req.repo, &branch, req.base, &title, &pr_body(req.report))
        .map_err(fail)
}

/// Real git + gh. Runs in the worker's repo (never the worktree: its push URL is blocked).
pub struct RealShipOps;

fn run(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn git(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo);
    c
}

fn gh(repo: &Path) -> Command {
    let mut c = Command::new("gh");
    c.current_dir(repo);
    c
}

impl ShipOps for RealShipOps {
    fn commits_ahead(&self, repo: &Path, base_ref: &str, branch: &str) -> Result<u32, String> {
        let range = format!("{base_ref}..{branch}");
        run(git(repo).args(["rev-list", "--count", &range]))?
            .parse()
            .map_err(|e| format!("rev-list count: {e}"))
    }

    fn tip_subject(&self, repo: &Path, branch: &str) -> Result<String, String> {
        run(git(repo).args(["log", "-1", "--format=%s", branch]))
    }

    fn push(&self, repo: &Path, branch: &str) -> Result<(), String> {
        run(git(repo).args(["push", "-u", "origin", branch])).map(|_| ())
    }

    fn open_pr(&self, repo: &Path, branch: &str) -> Result<Option<String>, String> {
        let url = run(gh(repo).args([
            "pr", "list", "--head", branch, "--state", "open", "--json", "url", "--jq", ".[0].url",
        ]))?;
        Ok((!url.is_empty()).then_some(url))
    }

    fn create_pr(
        &self,
        repo: &Path,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, String> {
        run(gh(repo).args([
            "pr", "create", "--head", branch, "--base", base, "--title", title, "--body", body,
        ]))
    }
}

/// CLI entry: `quaqua ship <worker> [--yes] [--repo <dir>] [--base <branch>]`.
pub fn handle_ship(args: &[String]) -> Result<(), (i32, String)> {
    let usage = || (1, format!("{USAGE}\n"));
    let (mut yes, mut repo, mut base, mut worker) = (false, None, "main".to_string(), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--yes" => yes = true,
            "--repo" => repo = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--base" => base = it.next().ok_or_else(usage)?.clone(),
            s if s.starts_with('-') || worker.is_some() => return Err(usage()),
            s => worker = Some(s.to_string()),
        }
    }
    let worker = worker.ok_or_else(usage)?;
    if worker.contains(['/', '\\']) || worker.starts_with('.') {
        return Err((1, format!("ship refused: bad worker name '{worker}'\n")));
    }
    let runs = crate::worker::cleanup::default_runs_dir();
    let report = std::fs::read_to_string(runs.join(format!("{worker}.md"))).map_err(|e| {
        (
            1,
            format!("ship refused: no report runs/{worker}.md: {e}\n"),
        )
    })?;
    let repo = match repo {
        Some(r) => r,
        None => std::fs::read_to_string(crate::state::WorkerMetadata::repo_path(&runs, &worker))
            .map(|s| PathBuf::from(s.trim()))
            .map_err(|e| {
                (
                    1,
                    format!("ship refused: no runs/{worker}.repo (pass --repo): {e}\n"),
                )
            })?,
    };
    let req = ShipRequest {
        worker: &worker,
        repo: &repo,
        base: &base,
        report: &report,
    };
    let url = if yes {
        ship(&RealShipOps, &AssumeYes, &req)?
    } else if std::io::stdin().is_terminal() {
        ship(&RealShipOps, &TtyPrompt, &req)?
    } else {
        return Err((
            1,
            "ship refused: no TTY for approval; the Principal must run it or pass --yes\n".into(),
        ));
    };
    println!("{url}");
    Ok(())
}
