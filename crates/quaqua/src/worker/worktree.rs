//! Git worktree lifecycle management for task workers.
//!
//! Handles worktree path resolution, commit-ish base verification,
//! creation/reuse of worktrees and task branches (`quaqua/<name>`),
//! push-blocking via per-worktree git configuration, worktree removal,
//! and dirty status checks.

use crate::git::GitOps;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Cap on each network git call made while resolving the start point, so an
/// offline spawn degrades to the local default instead of hanging.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Run `git -C <repo> <args>` with a deadline; `Some(stdout)` only on exit 0.
fn git_timed(repo: &Path, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    let _ = std::io::Read::read_to_string(&mut so, &mut out);
                }
                return st.success().then_some(out);
            }
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Default start point when no `--base` is given: fetch the remote's default
/// branch and return `refs/remotes/origin/<default>`. Fetch failure warns and
/// falls back to the local `<default>`; no origin or unknown default → `None`
/// (the repo's HEAD, as before). Never touches local branches.
fn default_start_point(repo: &Path) -> Option<String> {
    git_timed(repo, &["remote", "get-url", "origin"], FETCH_TIMEOUT)?;
    let default = git_timed(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        FETCH_TIMEOUT,
    )
    .and_then(|s| s.trim().strip_prefix("origin/").map(str::to_string))
    .or_else(|| {
        let out = git_timed(
            repo,
            &["ls-remote", "--symref", "origin", "HEAD"],
            FETCH_TIMEOUT,
        )?;
        let line = out.lines().find(|l| l.starts_with("ref: refs/heads/"))?;
        let name = line
            .strip_prefix("ref: refs/heads/")?
            .split_whitespace()
            .next()?;
        Some(name.to_string())
    })?;
    let local_fallback = || {
        let local = format!("refs/heads/{default}");
        git_timed(
            repo,
            &["rev-parse", "--verify", "--quiet", &local],
            FETCH_TIMEOUT,
        )
        .map(|_| local)
    };
    let tracking = format!("refs/remotes/origin/{default}");
    let refspec = format!("+refs/heads/{default}:{tracking}");
    if git_timed(
        repo,
        &["fetch", "--quiet", "origin", &refspec],
        FETCH_TIMEOUT,
    )
    .is_some()
    {
        Some(tracking)
    } else {
        let fb = local_fallback();
        eprintln!(
            "warning: fetch of origin/{default} failed — basing the worktree on {}",
            fb.as_deref().unwrap_or("HEAD")
        );
        fb
    }
}

/// Canonicalize a repository path, falling back to the original path if canonicalization fails.
pub fn canonicalize_repo_path(repo_path: &Path) -> PathBuf {
    match std::fs::canonicalize(repo_path) {
        Ok(p) => p,
        Err(_) => repo_path.to_path_buf(),
    }
}

/// Resolve the path to a worker's git worktree based on repo root and worker name.
///
/// Convention: `<repo.parent>/.wt/<name>`, or `.wt/<name>` if `repo` has no parent.
pub fn resolve_worktree_path(repo: &Path, name: &str) -> PathBuf {
    match repo.parent() {
        Some(p) => p.join(".wt").join(name),
        None => PathBuf::from(".wt").join(name),
    }
}

/// Convenience helper to canonicalize repo path and compute worktree path.
pub fn resolve_paths(repo_path: &Path, name: &str) -> (PathBuf, PathBuf) {
    let repo = canonicalize_repo_path(repo_path);
    let wt = resolve_worktree_path(&repo, name);
    (repo, wt)
}

/// Worker branch name convention: `quaqua/<name>`.
pub fn worker_branch_name(name: &str) -> String {
    format!("quaqua/{name}")
}

/// Validate that a `--base` ref exists and resolves to a commit-ish in the repository.
pub fn validate_base_ref<G: GitOps>(
    git: &G,
    repo: &Path,
    base_ref: &str,
) -> Result<(), (i32, String)> {
    if !git.rev_parse_commit(repo, base_ref) {
        return Err((
            1,
            format!(
                "error: --base '{base_ref}' is not a known commit-ish in {}\n",
                repo.display()
            ),
        ));
    }
    Ok(())
}

/// Block git pushes from a worker worktree by rewiring all remotes to disabled pushurls
/// using git's per-worktree configuration extensions.
///
/// Refuses if the common config has `core.bare=true` or `core.worktree` configured, as
/// enabling `extensions.worktreeConfig` in such repos breaks all worktrees.
pub fn apply_push_block<G: GitOps>(git: &G, repo: &Path, wt: &Path) -> Result<(), (i32, String)> {
    if git.is_core_bare(repo) || git.has_core_worktree(repo) {
        return Err((
            1,
            format!(
                "error: {} has core.bare=true or core.worktree in its common config; enabling extensions.worktreeConfig for the push block would break every worktree of it. Move those keys to the main worktree's config.worktree (see git-worktree(1), CONFIGURATION FILE) or spawn from another clone.\n",
                repo.display()
            ),
        ));
    }

    let _ = git.enable_worktree_config(repo);
    if let Ok(remotes) = git.remote_list(repo) {
        for remote in remotes {
            let _ = git.set_worktree_pushurl_blocked(wt, &remote);
        }
    }

    Ok(())
}

/// Prepare a git worktree for a task worker.
///
/// 1. If `wt` already exists as a directory:
///    - Checks `git worktree list --porcelain`. If registered, reuses it (resumed spawn).
///    - If not registered, refuses with an error.
/// 2. If `wt` does not exist:
///    - If branch `quaqua/<name>` already exists, reuses it with `worktree add` (same-name retry).
///    - Otherwise, creates the branch and worktree (`worktree add -b ...` starting at `base` or HEAD).
/// 3. Applies the push block to all remotes in the worktree.
pub fn prepare_worktree<G: GitOps>(
    git: &G,
    repo: &Path,
    wt: &Path,
    name: &str,
    base: Option<&str>,
) -> Result<(), (i32, String)> {
    if wt.is_dir() {
        let porcelain_wts = git.worktree_list_porcelain(repo).unwrap_or_default();
        let wt_str = wt.to_string_lossy().to_string();
        if porcelain_wts.iter().any(|w| w == &wt_str) {
            eprintln!(
                "note: {} already a registered worktree — reusing it (resumed spawn)",
                wt.display()
            );
        } else {
            return Err((
                1,
                format!(
                    "error: {} exists but is not a git worktree of {} — remove it or pick another name\n",
                    wt.display(),
                    repo.display()
                ),
            ));
        }
    } else {
        let branch = worker_branch_name(name);
        let add_out = if git.branch_exists(repo, &branch) {
            // cleanup-worker.sh drops the worktree but keeps the branch for the
            // Lead to merge, so a same-name retry after a failed (or cleaned)
            // spawn would die on `worktree add -b` with "a branch named
            // 'quaqua/<name>' already exists". Reuse the kept branch — the
            // retry resumes that task, commits included — and say so: a --base
            // cannot apply to a branch that already exists.
            let base_note = match base {
                Some(b) => format!("; --base {b} ignored"),
                None => String::new(),
            };
            eprintln!(
                "note: branch {branch} already exists — reusing it (same-name retry){base_note}"
            );
            git.worktree_add_existing(repo, &branch, wt)
        } else {
            let resolved = if base.is_none() {
                default_start_point(repo)
            } else {
                None
            };
            git.worktree_add(repo, &branch, wt, base.or(resolved.as_deref()))
        }
        .map_err(|e| (1, format!("error: failed to add worktree: {e}\n")))?;
        if !add_out.status.success() {
            let err_text = String::from_utf8_lossy(&add_out.stderr).to_string();
            return Err((1, err_text));
        }
    }

    apply_push_block(git, repo, wt)?;

    Ok(())
}

/// Alias for `prepare_worktree`.
pub use prepare_worktree as setup_worktree;

/// Remove a worker's git worktree.
pub fn remove_worktree<G: GitOps>(git: &G, repo: &Path, wt: &Path) -> Result<(), (i32, String)> {
    let rm_out = git
        .worktree_remove(repo, wt)
        .map_err(|e| (1, format!("error: worktree remove failed: {e}")))?;

    if !rm_out.status.success() {
        let err_text = String::from_utf8_lossy(&rm_out.stderr).to_string();
        return Err((rm_out.status.code().unwrap_or(1), err_text));
    }

    Ok(())
}

/// Check if a worktree has uncommitted or untracked changes.
pub fn is_worktree_dirty<G: GitOps>(git: &G, wt: &Path) -> Result<bool, String> {
    match git.run_in(wt, &["status", "--porcelain"]) {
        Ok(out) if out.status.success() => {
            let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
            Ok(!status.is_empty())
        }
        Ok(out) => Err(String::from_utf8_lossy(&out.stderr).to_string()),
        Err(e) => Err(e.to_string()),
    }
}
