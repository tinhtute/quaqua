//! Lead merge queue (`quaqua merge <slug> <branch>...`).
//! Implements piggery refinery merge queue.

use crate::config::load_team_config;
use crate::engine::journal_sqlite::{SqliteJournal, VerdictRecord};
use crate::git::{Git, GitOps};
use crate::workspace::workspace_root_checked;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const USAGE: &str = "\
# Usage: quaqua merge [--check <cmd>] [--repo <dir>] <slug> <branch>...
#   Merge one branch at a time into the project integration branch quaqua/<slug>-integ.
#   Runs project check after each merge. On failure or conflict, undoes via reset --hard
#   and returns rework to Peer. Never pushes and never rewrites integ beyond undo.
#
# Exit codes:
#   0: all branches merged cleanly
#   1: operational error (bad arguments, missing repo/branch, dirty worktree)
#   2: check failed with branch — returning rework to Peer (undo via reset --hard)
#   3: check fails on integ base without the branch — ask CoS and hold queue
#   4: merge conflict — returning rework to Peer (undo via merge --abort)";

pub struct MergeOptions {
    pub slug: String,
    pub branches: Vec<String>,
    pub check_cmd: Option<String>,
    pub repo: Option<PathBuf>,
}

pub fn parse_merge_args(args: &[String]) -> Result<MergeOptions, (i32, String)> {
    let mut check_cmd: Option<String> = None;
    let mut repo: Option<PathBuf> = None;
    let mut pos = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--check" {
            if i + 1 >= args.len() {
                return Err((1, format!("{}\n", USAGE)));
            }
            check_cmd = Some(args[i + 1].clone());
            i += 2;
        } else if let Some(v) = arg.strip_prefix("--check=") {
            check_cmd = Some(v.to_string());
            i += 1;
        } else if arg == "--repo" {
            if i + 1 >= args.len() {
                return Err((1, format!("{}\n", USAGE)));
            }
            repo = Some(PathBuf::from(&args[i + 1]));
            i += 2;
        } else if let Some(v) = arg.strip_prefix("--repo=") {
            repo = Some(PathBuf::from(v));
            i += 1;
        } else if arg.starts_with('-') {
            return Err((1, format!("{}\n", USAGE)));
        } else {
            pos.push(arg.clone());
            i += 1;
        }
    }

    if pos.len() < 2 {
        return Err((1, format!("{}\n", USAGE)));
    }

    Ok(MergeOptions {
        slug: pos[0].clone(),
        branches: pos[1..].to_vec(),
        check_cmd,
        repo,
    })
}

/// Parses project slug and repo paths from a `projects/INDEX.md` file (§ S2 fix).
pub fn parse_project_index_rows(index: &Path) -> Vec<(String, PathBuf)> {
    let Ok(content) = std::fs::read_to_string(index) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('|') {
            let parts: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();
            if parts.len() >= 3 && parts[1] != "slug" && parts[2] != "repo" && !parts[1].is_empty()
            {
                let slug = parts[1].to_string();
                let r = parts[2];
                let p = if let Some(stripped) = r.strip_prefix("~/") {
                    format!("{}/{}", home, stripped)
                } else if r == "~" {
                    home.clone()
                } else {
                    r.to_string()
                };
                rows.push((slug, PathBuf::from(p)));
            }
        }
    }
    rows
}

/// Resolves all valid project repository paths from a workspace root (§ S2 fix).
pub fn resolve_project_repos_from_workspace(ws_root: &Path) -> Vec<PathBuf> {
    let index = ws_root.join("projects/INDEX.md");
    let mut repos = Vec::new();
    for (_slug, pb) in parse_project_index_rows(&index) {
        if pb.is_dir() && !repos.contains(&pb) {
            repos.push(pb);
        }
    }
    repos
}

/// Resolves all valid project repository paths from `projects/INDEX.md` (§ S2 fix).
pub fn resolve_project_repos_from_index() -> Vec<PathBuf> {
    if let Ok(root) = workspace_root_checked() {
        return resolve_project_repos_from_workspace(&root);
    }
    Vec::new()
}

fn resolve_project_repo(
    slug: &str,
    explicit_repo: Option<&Path>,
) -> Result<PathBuf, (i32, String)> {
    if let Some(r) = explicit_repo {
        if r.is_dir() {
            return Ok(r.to_path_buf());
        } else {
            return Err((
                1,
                format!(
                    "repo path '{}' does not exist or is not a directory\n",
                    r.display()
                ),
            ));
        }
    }

    let root = workspace_root_checked()?;
    let index = root.join("projects/INDEX.md");
    if !index.is_file() {
        return Err((
            1,
            format!("cannot resolve project repo for slug '{slug}'\n"),
        ));
    }
    let rows = parse_project_index_rows(&index);
    for (s, pb) in rows {
        if s == slug {
            if pb.is_dir() {
                return Ok(pb);
            } else {
                return Err((
                    1,
                    format!(
                        "project repo '{}' for slug '{slug}' does not exist\n",
                        pb.display()
                    ),
                ));
            }
        }
    }
    Err((
        1,
        format!(
            "unknown project slug '{slug}' — no row in {}\n",
            index.display()
        ),
    ))
}

fn find_worktree_for_branch(repo: &Path, branch_name: &str) -> Option<PathBuf> {
    let git = Git::new();
    let out = git
        .run_in(repo, &["worktree", "list", "--porcelain"])
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut cur_wt: Option<PathBuf> = None;
    let branch_ref = format!("refs/heads/{branch_name}");
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            cur_wt = Some(PathBuf::from(path.trim()));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b.trim() == branch_ref {
                return cur_wt;
            }
        }
    }
    None
}

/// Inspects the strongest verdict for `branch` at its head SHA and warns if missing (§ S2).
pub fn check_branch_verdict<G: GitOps>(
    db_path: &Path,
    target: &Path,
    branch: &str,
    git: &G,
) -> Option<VerdictRecord> {
    check_branch_verdict_with_sink(db_path, target, branch, git, &mut std::io::stderr())
}

/// Inspects the strongest verdict for `branch` at its head SHA, writing warnings to `warn_sink` (§ S2 fix).
pub fn check_branch_verdict_with_sink<G: GitOps, W: std::io::Write>(
    db_path: &Path,
    target: &Path,
    branch: &str,
    git: &G,
    warn_sink: &mut W,
) -> Option<VerdictRecord> {
    let branch_head = git
        .run_in(target, &["rev-parse", "--verify", "--quiet", branch])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .or_else(|| {
            let ref_name = format!("refs/heads/{branch}");
            git.run_in(target, &["rev-parse", "--verify", "--quiet", &ref_name])
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_default();

    if branch_head.is_empty() {
        let _ = writeln!(
            warn_sink,
            "warning: no verdict for branch '{branch}' (unresolvable head)"
        );
        return None;
    }

    if !db_path.exists() {
        let _ = writeln!(
            warn_sink,
            "warning: no verdict matches head SHA {branch_head} for branch '{branch}'"
        );
        return None;
    }

    let journal = match SqliteJournal::open(db_path) {
        Ok(j) => j,
        Err(e) => {
            let _ = writeln!(
                warn_sink,
                "warning: cannot open journal {}: {e}",
                db_path.display()
            );
            let _ = writeln!(
                warn_sink,
                "warning: no verdict matches head SHA {branch_head} for branch '{branch}'"
            );
            return None;
        }
    };

    let verdict = journal
        .strongest_verdict(branch, &branch_head)
        .ok()
        .flatten()
        .or_else(|| {
            if !branch.starts_with("quaqua/") {
                journal
                    .strongest_verdict(&format!("quaqua/{branch}"), &branch_head)
                    .ok()
                    .flatten()
            } else {
                journal
                    .strongest_verdict(branch.strip_prefix("quaqua/").unwrap(), &branch_head)
                    .ok()
                    .flatten()
            }
        });

    match &verdict {
        Some(v) => {
            println!("verdict for '{branch}' at {branch_head}: {}", v.level);
        }
        None => {
            let _ = writeln!(
                warn_sink,
                "warning: no verdict matches head SHA {branch_head} for branch '{branch}'"
            );
        }
    }

    verdict
}

struct MergeWorktreeGuard<'a> {
    repo: &'a Path,
    target_dir: PathBuf,
    is_temp_worktree: bool,
    orig_branch: Option<String>,
    integ: String,
}

impl<'a> Drop for MergeWorktreeGuard<'a> {
    fn drop(&mut self) {
        let git = Git::new();
        if self.is_temp_worktree {
            let _ = git.run_in(
                self.repo,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    &self.target_dir.to_string_lossy(),
                ],
            );
            let _ = std::fs::remove_dir_all(&self.target_dir);
        } else if let Some(orig) = &self.orig_branch {
            if orig != &self.integ && !orig.is_empty() && orig != "HEAD" {
                let _ = git.run_in(self.repo, &["checkout", "--quiet", orig]);
            }
        }
    }
}

/// Default build check for a Rust `*-integ` target when no check is configured (#170):
/// a merge that is clean as text can still fail to compile.
/// `QUAQUA_INTEG_CHECK_CMD` replaces the command (tests inject a fake).
const INTEG_CHECK_CMD: &str = "cargo check --workspace --locked";

fn resolve_check_cmd(explicit: Option<&str>, integ: &str, target: &Path) -> Option<String> {
    let configured = explicit
        .map(str::to_string)
        .or_else(|| std::env::var("QUAQUA_CHECK_CMD").ok())
        .or_else(|| std::env::var("MERGE_CHECK_CMD").ok());
    if let Some(cmd) = configured {
        return (!cmd.trim().is_empty()).then_some(cmd);
    }
    if integ.ends_with("-integ") && target.join("Cargo.toml").is_file() {
        return Some(
            std::env::var("QUAQUA_INTEG_CHECK_CMD").unwrap_or_else(|_| INTEG_CHECK_CMD.to_string()),
        )
        .filter(|cmd| !cmd.trim().is_empty());
    }
    None
}

/// A check passes only if it exits 0 and leaves tracked files untouched: a modified
/// tracked file (e.g. a rewritten Cargo.lock) would otherwise make the next merge fail
/// as a misattributed conflict.
fn run_check(repo: &Path, check_cmd: Option<&str>) -> bool {
    let Some(cmd_str) = check_cmd else {
        return true;
    };
    let ok = Command::new("sh")
        .arg("-c")
        .arg(cmd_str)
        .current_dir(repo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok && Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(repo)
        .output()
        .map(|o| o.status.success() && o.stdout.is_empty())
        .unwrap_or(false)
}

pub fn handle_merge(args: &[String], git: &Git) -> Result<(), (i32, String)> {
    handle_merge_with_sink(args, git, &mut std::io::stderr())
}

pub fn handle_merge_with_sink<W: std::io::Write>(
    args: &[String],
    git: &Git,
    warn_sink: &mut W,
) -> Result<(), (i32, String)> {
    let opts = parse_merge_args(args)?;
    let cfg = load_team_config()?;
    let repo = resolve_project_repo(&opts.slug, opts.repo.as_deref())?;

    let integ_pattern = if cfg.merge.integ_branch.is_empty() {
        "quaqua/{slug}-integ"
    } else {
        &cfg.merge.integ_branch
    };
    let integ = integ_pattern.replace("{slug}", &opts.slug);

    // Verify integration branch exists
    let integ_ref = format!("refs/heads/{}", integ);
    if !git
        .run_in(&repo, &["rev-parse", "--verify", "--quiet", &integ_ref])
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Err((
            1,
            format!(
                "integration branch '{integ}' does not exist in {}\n",
                repo.display()
            ),
        ));
    }

    // Determine working worktree without mutating the primary checkout unnecessarily
    let existing_wt = find_worktree_for_branch(&repo, &integ);
    let guard = if let Some(wt) = existing_wt {
        // integ is already checked out in a worktree: use it directly
        // Verify its worktree is clean
        let st_out = git
            .run_in(&wt, &["status", "--porcelain"])
            .map_err(|e| (1, format!("failed to check status: {e}\n")))?;
        let dirty = String::from_utf8_lossy(&st_out.stdout).trim().to_string();
        if !dirty.is_empty() {
            return Err((
                1,
                format!(
                    "worktree in {} is dirty — cannot run merge queue\n",
                    wt.display()
                ),
            ));
        }
        MergeWorktreeGuard {
            repo: &repo,
            target_dir: wt,
            is_temp_worktree: false,
            orig_branch: None,
            integ: integ.clone(),
        }
    } else {
        // Try creating a temporary worktree on integ
        let tmp_dir =
            std::env::temp_dir().join(format!("quaqua-merge-{}-{}", opts.slug, std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let add_out = git.run_in(
            &repo,
            &["worktree", "add", &tmp_dir.to_string_lossy(), &integ],
        );

        match add_out {
            Ok(o) if o.status.success() => MergeWorktreeGuard {
                repo: &repo,
                target_dir: tmp_dir,
                is_temp_worktree: true,
                orig_branch: None,
                integ: integ.clone(),
            },
            _ => {
                // Fallback: use primary repo worktree with checkout guard
                let st_out = git
                    .run_in(&repo, &["status", "--porcelain"])
                    .map_err(|e| (1, format!("failed to check status: {e}\n")))?;
                let dirty = String::from_utf8_lossy(&st_out.stdout).trim().to_string();
                if !dirty.is_empty() {
                    return Err((
                        1,
                        format!(
                            "worktree in {} is dirty — cannot run merge queue\n",
                            repo.display()
                        ),
                    ));
                }

                let orig_branch = git
                    .run_in(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();

                let co_out = git
                    .run_in(&repo, &["checkout", "--quiet", &integ])
                    .map_err(|e| (1, format!("failed to checkout {integ}: {e}\n")))?;
                if !co_out.status.success() {
                    let err = String::from_utf8_lossy(&co_out.stderr).to_string();
                    return Err((1, format!("failed to checkout {integ}: {err}\n")));
                }

                MergeWorktreeGuard {
                    repo: &repo,
                    target_dir: repo.clone(),
                    is_temp_worktree: false,
                    orig_branch: Some(orig_branch),
                    integ: integ.clone(),
                }
            }
        }
    };

    let target = &guard.target_dir;

    let check_cmd = resolve_check_cmd(opts.check_cmd.as_deref(), &integ, target);
    if check_cmd.is_none() {
        let _ = writeln!(
            warn_sink,
            "warning: no check command configured; merging without checks"
        );
    }
    let db_path = crate::worker::cleanup::default_runs_dir().join("quaqua.db");

    for branch in &opts.branches {
        check_branch_verdict_with_sink(&db_path, target, branch, git, warn_sink);

        // 1. Note the integ commit
        let rev_out = git
            .run_in(target, &["rev-parse", "HEAD"])
            .map_err(|e| (1, format!("failed to rev-parse HEAD: {e}\n")))?;
        let head_before = String::from_utf8_lossy(&rev_out.stdout).trim().to_string();

        // 2. Try merging
        let mut merge_cmd = Command::new("git");
        merge_cmd
            .arg("-C")
            .arg(target)
            .arg("merge")
            .arg("--no-edit")
            .arg(branch);
        if std::env::var("GIT_AUTHOR_NAME").is_err() {
            merge_cmd.env("GIT_AUTHOR_NAME", "quaqua");
            merge_cmd.env("GIT_AUTHOR_EMAIL", "quaqua@local");
            merge_cmd.env("GIT_COMMITTER_NAME", "quaqua");
            merge_cmd.env("GIT_COMMITTER_EMAIL", "quaqua@local");
        }
        let merge_out = merge_cmd
            .output()
            .map_err(|e| (1, format!("failed to run git merge: {e}\n")))?;

        if !merge_out.status.success() {
            // Conflict -> merge --abort (exit code 4 for conflict rework)
            let _ = git.run_in(target, &["merge", "--abort"]);
            return Err((
                4,
                format!("error: merge conflict merging '{branch}' into '{integ}' — returning rework to Peer\n"),
            ));
        }

        // 3. Run project check
        let check_pass = run_check(target, check_cmd.as_deref());
        if check_pass {
            let new_rev = git
                .run_in(target, &["rev-parse", "HEAD"])
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            println!("merged {branch} into {integ} ({new_rev})");
        } else {
            // Undo merge: git reset --hard <head_before>
            let reset_out = git
                .run_in(target, &["reset", "--hard", &head_before])
                .map_err(|e| (1, format!("failed to reset --hard: {e}\n")))?;
            if !reset_out.status.success() {
                let err = String::from_utf8_lossy(&reset_out.stderr).to_string();
                return Err((1, format!("error: reset --hard failed: {err}\n")));
            }

            // Check if check fails without the branch too
            let base_check_pass = run_check(target, check_cmd.as_deref());
            if !base_check_pass {
                return Err((
                    3,
                    format!("error: check `{}` fails (or modifies tracked files) on integ base without '{branch}' — ask CoS and hold queue\n", check_cmd.as_deref().unwrap_or("")),
                ));
            } else {
                return Err((
                    2,
                    format!("error: check `{}` failed (or modified tracked files) with branch '{branch}' merged into '{integ}' — undoing merge and returning rework to Peer\n", check_cmd.as_deref().unwrap_or("")),
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::journal_sqlite::{
        SqliteJournal, VerdictLevel, VerdictRecord, VerdictSource,
    };
    use std::fs;

    #[test]
    fn test_merge_warns_on_head_with_no_verdict_exit_code_unchanged() {
        let tmp = std::env::temp_dir().join(format!("test-merge-verdict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        let runs = tmp.join("runs");
        let projects = tmp.join("projects");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&runs).unwrap();
        fs::create_dir_all(&projects).unwrap();

        let git = Git::new();
        let _ = git.run_in(&repo, &["init", "-q"]).unwrap();
        let _ = git
            .run_in(&repo, &["config", "user.email", "test@test"])
            .unwrap();
        let _ = git.run_in(&repo, &["config", "user.name", "test"]).unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "initial"])
            .unwrap();
        let base =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();

        // Create integ branch quaqua/proj-integ
        let _ = git
            .run_in(&repo, &["branch", "quaqua/proj-integ", &base])
            .unwrap();
        let _ = git
            .run_in(&repo, &["checkout", "-q", "quaqua/proj-integ"])
            .unwrap();

        // Create branch-unverified
        let _ = git
            .run_in(&repo, &["checkout", "-qb", "branch-unverified", &base])
            .unwrap();
        fs::write(repo.join("file_unverified.txt"), "hello\n").unwrap();
        let _ = git.run_in(&repo, &["add", "."]).unwrap();
        let _ = git
            .run_in(&repo, &["commit", "-qm", "unverified work"])
            .unwrap();

        let db_path = runs.join("quaqua.db");

        // 1. check_branch_verdict on branch-unverified: returns None
        let v_opt = check_branch_verdict(&db_path, &repo, "branch-unverified", &git);
        assert!(v_opt.is_none(), "unverified branch must return no verdict");

        // 2. Create branch-verified with a verdict recorded at its head
        let _ = git
            .run_in(&repo, &["checkout", "-qb", "branch-verified", &base])
            .unwrap();
        fs::write(repo.join("file_verified.txt"), "world\n").unwrap();
        let _ = git.run_in(&repo, &["add", "."]).unwrap();
        let _ = git
            .run_in(&repo, &["commit", "-qm", "verified work"])
            .unwrap();
        let sha_verified =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();

        let mut journal = SqliteJournal::open(&db_path).expect("open journal");
        let record = VerdictRecord {
            branch: "branch-verified".to_string(),
            sha: sha_verified.clone(),
            level: VerdictLevel::ReviewClean,
            evidence: "clean review".to_string(),
            verifier: "suhuynh-rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("suhuynh".to_string()),
            harness: "pi".to_string(),
        };
        journal.record_verdict("wf-merge-test", &record).unwrap();

        let v_verified = check_branch_verdict(&db_path, &repo, "branch-verified", &git);
        assert!(v_verified.is_some(), "verified branch must return verdict");
        let v = v_verified.unwrap();
        assert_eq!(v.sha, sha_verified);
        assert_eq!(v.level, VerdictLevel::ReviewClean);

        // 3. Test handle_merge on branch-unverified: exit code remains 0
        fs::write(projects.join("INDEX.md"), format!("# Projects\n| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| proj | {} | cos | active | goal |\n", repo.display())).unwrap();
        fs::write(projects.join("proj.md"), "# proj\n").unwrap();
        fs::write(
            tmp.join("team.toml"),
            include_str!("../../../../templates/workspace/team.toml"),
        )
        .unwrap();

        let _env_lock = crate::test_env::lock_env();
        let _env_guard = EnvGuard::capture(&["QUAQUA_HOME", "QUAQUA_RUNS_DIR"]);
        std::env::set_var("QUAQUA_HOME", &tmp);
        std::env::set_var("QUAQUA_RUNS_DIR", &runs);
        let args = vec![
            "--check".to_string(),
            "true".to_string(),
            "--repo".to_string(),
            repo.to_str().unwrap().to_string(),
            "proj".to_string(),
            "branch-unverified".to_string(),
        ];
        let mut warn_buf = Vec::new();
        let res = handle_merge_with_sink(&args, &git, &mut warn_buf);
        assert!(
            res.is_ok(),
            "merge of unverified branch must succeed (warn only, exit code unchanged): {:?}",
            res.err()
        );
        let warn_str = String::from_utf8_lossy(&warn_buf);
        assert!(
            warn_str.contains("warning: no verdict matches head SHA"),
            "warning must be emitted to sink on unverified head, got: {warn_str}"
        );
    }

    /// #170: a `*-integ` Rust target with no configured check runs the build check after
    /// each merge; a failing build stops the queue before the next branch.
    #[test]
    fn test_integ_default_build_check_stops_queue() {
        let _env_lock = crate::test_env::lock_env();
        let _env_guard = EnvGuard::capture(&[
            "QUAQUA_HOME",
            "QUAQUA_RUNS_DIR",
            "QUAQUA_CHECK_CMD",
            "MERGE_CHECK_CMD",
            "QUAQUA_INTEG_CHECK_CMD",
        ]);
        std::env::remove_var("QUAQUA_CHECK_CMD");
        std::env::remove_var("MERGE_CHECK_CMD");
        let git = Git::new();
        // (case, fake check, expected exit code; 0 = all merged)
        let cases = [
            // Passes on the base, fails once b1 is merged: rework, b1 undone.
            ("rework", "test ! -f b1.txt", 2),
            // Passes, but rewrites a tracked file once b1 is merged (a stale merged
            // Cargo.lock): rework for b1, never a misattributed merge conflict.
            (
                "dirty",
                "if [ -f b1.txt ]; then echo '# x' >> Cargo.toml; fi",
                2,
            ),
            ("pass", "true", 0),
        ];
        for (case, fake, want) in cases {
            let tmp = std::env::temp_dir()
                .join(format!("test-merge-integ-{}-{case}", std::process::id()));
            let _ = fs::remove_dir_all(&tmp);
            let repo = tmp.join("repo");
            let runs = tmp.join("runs");
            fs::create_dir_all(&repo).unwrap();
            fs::create_dir_all(&runs).unwrap();
            fs::write(
                tmp.join("team.toml"),
                include_str!("../../../../templates/workspace/team.toml"),
            )
            .unwrap();
            let g = |a: &[&str]| assert!(git.run_in(&repo, a).unwrap().status.success(), "{a:?}");
            g(&["init", "-q", "-b", "main"]);
            g(&["config", "user.email", "t@t"]);
            g(&["config", "user.name", "t"]);
            fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
            g(&["add", "."]);
            g(&["commit", "-qm", "base"]);
            g(&["branch", "quaqua/proj-integ"]);
            for b in ["b1", "b2"] {
                g(&["checkout", "-qb", b, "main"]);
                fs::write(repo.join(format!("{b}.txt")), b).unwrap();
                g(&["add", "."]);
                g(&["commit", "-qm", b]);
            }
            g(&["checkout", "-q", "main"]);
            std::env::set_var("QUAQUA_HOME", &tmp);
            std::env::set_var("QUAQUA_RUNS_DIR", &runs);
            std::env::set_var("QUAQUA_INTEG_CHECK_CMD", fake);
            let args: Vec<String> = ["--repo", repo.to_str().unwrap(), "proj", "b1", "b2"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let mut warn = Vec::new();
            let res = handle_merge_with_sink(&args, &git, &mut warn);
            let merged = |b: &str| {
                git.run_in(
                    &repo,
                    &["merge-base", "--is-ancestor", b, "quaqua/proj-integ"],
                )
                .unwrap()
                .status
                .success()
            };
            if want == 0 {
                assert!(res.is_ok(), "passing check must merge all: {res:?}");
                assert!(merged("b1") && merged("b2"));
            } else {
                let Err((code, msg)) = res else {
                    panic!("{case}: failing check must fail the merge")
                };
                assert_eq!(code, want, "{case}: {msg}");
                assert!(
                    msg.contains(fake),
                    "{case}: message must name the check: {msg}"
                );
                assert!(!merged("b1"), "{case}: failing branch must be undone");
                assert!(
                    !merged("b2"),
                    "{case}: queue must stop before the next branch"
                );
            }
            let _ = fs::remove_dir_all(&tmp);
        }
    }
    struct EnvGuard {
        vars: Vec<(String, Option<String>)>,
    }
    impl EnvGuard {
        fn capture(keys: &[&str]) -> Self {
            let vars = keys
                .iter()
                .map(|k| (k.to_string(), std::env::var(k).ok()))
                .collect();
            Self { vars }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.vars {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}
