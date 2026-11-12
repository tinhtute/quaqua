//! Verdict in the journal (§ S1).
//!
//! Handles parsing builder self-reports (`Verified: <level> <sha>`) and reviewer
//! verdicts (`Verdict: clean | <N> findings`), validating them against worktree HEAD
//! or the H4 baseline SHA, and recording deduplicated `verdict` events into the
//! journal SQLite database (`runs/quaqua.db`).
//!
//! A failure of the journal (locked, missing, or open failure) warns and never
//! turns into a block.

use std::path::{Path, PathBuf};
use std::str::FromStr;

pub use crate::engine::journal_sqlite::{
    SqliteJournal, VerdictLevel, VerdictRecord, VerdictSource,
};
use crate::git::GitOps;
use crate::state::WorkerMetadata;
use crate::worker::prompt::readonly;
use crate::worker::worktree::resolve_worktree_path;

/// The last `Verified:` line of a builder report: `(raw line, level, sha)`.
///
/// Bullet markers and a bold `**Verified:**` label are stripped from `raw`; the
/// remaining words are split into the level token and the reported SHA.
fn last_verified_line(report_content: &str) -> Option<(&str, &str, &str)> {
    let (raw_line, rest) = report_content
        .lines()
        .map(str::trim)
        .filter_map(|l| {
            let mut s = l;
            if let Some(rest) = s.strip_prefix("- ").or_else(|| s.strip_prefix("* ")) {
                s = rest.trim();
            }
            if let Some(rest) = s
                .strip_prefix("**Verified:**")
                .or_else(|| s.strip_prefix("**verified:**"))
            {
                Some((s, rest.trim()))
            } else if let Some(rest) = s
                .strip_prefix("Verified:")
                .or_else(|| s.strip_prefix("verified:"))
            {
                Some((s, rest.trim()))
            } else {
                None
            }
        })
        .next_back()?;
    let mut parts = rest.split_whitespace();
    let level = parts.next().unwrap_or("");
    let sha = parts.next().unwrap_or("");
    Some((raw_line, level, sha))
}

/// Warning text for a `Verified:` level the parser does not recognise. The
/// builder is told what was recorded and which tokens are valid.
pub fn unknown_level_warning(level: &str) -> String {
    format!(
        "unrecognised Verified level '{level}' in builder report \
         (rules/_all/brief.md); recorded as `inconclusive`. Valid levels: {}",
        VerdictLevel::vocabulary()
    )
}

/// Parses a builder report line `Verified: <level> <sha>`.
///
/// Compares `<sha>` with the actual worktree HEAD:
/// - Mismatch or missing -> `VerdictLevel::Inconclusive`
/// - Unknown level -> `VerdictLevel::Inconclusive`
/// - Valid level and matching SHA -> `<level>` at `worktree_head`
pub fn parse_builder_report(
    report_content: &str,
    worktree_head: &str,
) -> (VerdictLevel, String, String) {
    if let Some((raw_line, level_str, reported_sha)) = last_verified_line(report_content) {
        let parsed_level = VerdictLevel::from_str(level_str).unwrap_or(VerdictLevel::Inconclusive);

        let matches_head = !worktree_head.is_empty()
            && !reported_sha.is_empty()
            && (reported_sha == worktree_head
                || (reported_sha.len() >= 7 && worktree_head.starts_with(reported_sha)));

        if matches_head {
            (
                parsed_level,
                worktree_head.to_string(),
                raw_line.to_string(),
            )
        } else {
            let sha = if !worktree_head.is_empty() {
                worktree_head.to_string()
            } else {
                reported_sha.to_string()
            };
            (VerdictLevel::Inconclusive, sha, raw_line.to_string())
        }
    } else {
        (
            VerdictLevel::Inconclusive,
            worktree_head.to_string(),
            "no Verified: line in report".to_string(),
        )
    }
}

/// Parses a reviewer report verdict line (`Verdict: clean | <N> findings | blocked`).
pub fn parse_reviewer_report(report_content: &str) -> (VerdictLevel, String) {
    let last_verdict = report_content
        .lines()
        .map(str::trim)
        .filter_map(|l| {
            let mut s = l;
            if let Some(rest) = s.strip_prefix("- ").or_else(|| s.strip_prefix("* ")) {
                s = rest.trim();
            }
            if let Some(rest) = s
                .strip_prefix("**Verdict:**")
                .or_else(|| s.strip_prefix("**verdict:**"))
            {
                Some((s, rest.trim()))
            } else if let Some(rest) = s
                .strip_prefix("Verdict:")
                .or_else(|| s.strip_prefix("verdict:"))
            {
                Some((s, rest.trim()))
            } else {
                None
            }
        })
        .next_back();

    if let Some((raw_line, rest)) = last_verdict {
        let vline = format!("Verdict: {rest}");
        let lvl = match crate::pipeline::parse_verdict_str(&vline) {
            Ok((0, _)) => VerdictLevel::ReviewClean,
            Ok((n, _)) if n > 0 => VerdictLevel::VerifierFailed,
            _ => VerdictLevel::Inconclusive,
        };
        (lvl, raw_line.to_string())
    } else {
        (
            VerdictLevel::Inconclusive,
            "no Verdict: line in report".to_string(),
        )
    }
}

/// Resolves the worktree directory for worker `name`.
pub fn resolve_worker_worktree(
    runs_dir: &Path,
    name: &str,
    repo_dir: Option<&Path>,
) -> Option<PathBuf> {
    // 1. If readonly baseline exists, line 1 is wt
    if let Some((wt, _)) = readonly::baseline_info(runs_dir, name) {
        if wt.is_dir() {
            return Some(wt);
        }
    }

    // 2. Resolve via repo_dir
    if let Some(repo) = repo_dir {
        let wt = resolve_worktree_path(repo, name);
        if wt.is_dir() {
            return Some(wt);
        }
    }

    // 3. Resolve via workspace runs_dir.parent()
    if let Some(root) = runs_dir.parent() {
        let wt = root.join(".wt").join(name);
        if wt.is_dir() {
            return Some(wt);
        }
        let wt_direct = resolve_worktree_path(root, name);
        if wt_direct.is_dir() {
            return Some(wt_direct);
        }
    }

    None
}

/// Records a verdict event in the journal upon successful acceptance of a report.
///
/// Deduplication: skips append if `(branch, sha, source, verifier)` already exists.
/// Journal failure (locked, missing, open failure) emits a warning and returns `Ok(None)`
/// or `Ok(Some(...))`, NEVER failing acceptance.
pub fn record_verdict_on_acceptance<G: GitOps>(
    runs_dir: &Path,
    name: &str,
    report_content: &str,
    expect_verdict: bool,
    git: &G,
    repo_dir: Option<&Path>,
) -> Result<Option<VerdictRecord>, String> {
    record_verdict_on_acceptance_warned(
        runs_dir,
        name,
        report_content,
        expect_verdict,
        git,
        repo_dir,
        &mut |msg: &str| eprintln!("warning: {msg}"),
    )
}

/// [`record_verdict_on_acceptance`] with the warning sink passed in, so a test
/// can read the warnings the accept path emits instead of scraping stderr.
#[allow(clippy::too_many_arguments)]
fn record_verdict_on_acceptance_warned<G: GitOps>(
    runs_dir: &Path,
    name: &str,
    report_content: &str,
    expect_verdict: bool,
    git: &G,
    repo_dir: Option<&Path>,
    warn: &mut dyn FnMut(&str),
) -> Result<Option<VerdictRecord>, String> {
    // Exclude taskforce workers from recording branch review verdicts
    if runs_dir.join(format!("{name}.taskforce")).is_file() {
        return Ok(None);
    }

    // Exclude unparseable reviewer reports from recording verdicts (warn and skip)
    if expect_verdict {
        if let Err(reason) = crate::pipeline::parse_report_content(report_content) {
            eprintln!("warning: reviewer verdict unparseable ({reason}), skipping verdict record");
            return Ok(None);
        }
    }

    let kind = name.split('-').next().unwrap_or(name);
    let team_cfg = crate::config::load_team_config().ok();
    let harness = team_cfg
        .as_ref()
        .and_then(|cfg| cfg.kinds.get(kind))
        .map(|k| k.harness.clone())
        .unwrap_or_default();

    let wt_opt = resolve_worker_worktree(runs_dir, name, repo_dir);

    let (branch, sha, level, evidence, source, builder_kind, verifier_kind) = if expect_verdict {
        // Reviewer
        let (mut lvl, ev) = parse_reviewer_report(report_content);

        // SHA from H4 baseline
        let baseline_sha = readonly::baseline_sha(runs_dir, name).unwrap_or_default();
        let head_sha = if !baseline_sha.is_empty() {
            baseline_sha
        } else if let Some(wt) = &wt_opt {
            git.run_in(wt, &["rev-parse", "HEAD"])
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        // F5 guard: a reviewer verdict with no resolvable SHA is never journalled as a pass
        if head_sha.is_empty() {
            lvl = VerdictLevel::Inconclusive;
        }

        // Branch & builder_kind from pipeline state if available
        let pipeline_task_opt = WorkerMetadata::load_pipeline(runs_dir, name);
        let mut b_kind: Option<String> = None;
        let mut b_name = String::new();

        if let Some(pipeline_task) = pipeline_task_opt {
            if !pipeline_task.is_empty() {
                let pstate_file = runs_dir.join(format!("pipeline-{pipeline_task}.state"));
                if let Ok(content) = std::fs::read_to_string(&pstate_file) {
                    for line in content.lines() {
                        if let Some(b) = line.strip_prefix("builder=") {
                            let b_trimmed = b.trim();
                            if !b_trimmed.is_empty() {
                                b_kind = Some(b_trimmed.to_string());
                                b_name = format!("quaqua/{b_trimmed}-{pipeline_task}");
                                break;
                            }
                        }
                    }
                }
            }
        }
        // If no pipeline state, check for persisted base ref (F1)
        if b_name.is_empty() {
            let base_file = runs_dir.join(format!("{name}.base"));
            if let Ok(base_content) = std::fs::read_to_string(&base_file) {
                let base_ref = base_content.trim();
                if !base_ref.is_empty() {
                    let branch_name = if let Some(stripped) = base_ref.strip_prefix("refs/heads/") {
                        stripped
                    } else {
                        base_ref
                    };
                    b_name = if branch_name.starts_with("quaqua/") {
                        branch_name.to_string()
                    } else {
                        format!("quaqua/{branch_name}")
                    };
                    let worker_part = b_name.strip_prefix("quaqua/").unwrap_or(&b_name);
                    let k = worker_part.split('-').next().unwrap_or(worker_part);
                    if !k.is_empty() {
                        b_kind = Some(k.to_string());
                    }
                }
            }
        }

        let branch = if !b_name.is_empty() {
            b_name
        } else if let Some(wt) = &wt_opt {
            git.run_in(wt, &["branch", "--show-current"])
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("quaqua/{name}"))
        } else {
            format!("quaqua/{name}")
        };

        (
            branch,
            head_sha,
            lvl,
            ev,
            VerdictSource::Verifier,
            b_kind,
            Some(kind.to_string()),
        )
    } else {
        // Builder
        let worktree_head = if let Some(wt) = &wt_opt {
            git.run_in(wt, &["rev-parse", "HEAD"])
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let branch = if let Some(wt) = &wt_opt {
            git.run_in(wt, &["branch", "--show-current"])
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("quaqua/{name}"))
        } else {
            format!("quaqua/{name}")
        };

        let (lvl, s_sha, ev) = parse_builder_report(report_content, &worktree_head);
        // A level the parser does not know records as `inconclusive`; say so and
        // name the vocabulary instead of leaving the builder to guess. Warns,
        // never blocks (pstack MVP).
        if let Some((_, level_token, _)) = last_verified_line(report_content) {
            if VerdictLevel::parse_known(level_token).is_none() {
                warn(&unknown_level_warning(level_token));
            }
        }

        (
            branch,
            s_sha,
            lvl,
            ev,
            VerdictSource::SelfReport,
            Some(kind.to_string()),
            None,
        )
    };

    let verdict = VerdictRecord {
        branch,
        sha,
        level,
        evidence,
        verifier: name.to_string(),
        source,
        builder_kind,
        verifier_kind,
        harness,
    };

    let db_path = runs_dir.join("quaqua.db");
    match SqliteJournal::open(&db_path) {
        Ok(mut journal) => {
            let wf_id = format!("worker:{name}");
            if let Err(e) = journal.record_verdict(&wf_id, &verdict) {
                eprintln!(
                    "warning: failed to record verdict in journal at {}: {e}",
                    db_path.display()
                );
            }
        }
        Err(e) => {
            eprintln!("warning: journal unavailable at {}: {e}", db_path.display());
        }
    }

    Ok(Some(verdict))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    struct MockGit {
        head_sha: String,
        branch: String,
        wt_dir: Option<PathBuf>,
        wt_branch: Option<String>,
    }

    impl MockGit {
        fn new(head_sha: &str, branch: &str) -> Self {
            Self {
                head_sha: head_sha.to_string(),
                branch: branch.to_string(),
                wt_dir: None,
                wt_branch: None,
            }
        }

        fn with_wt(head_sha: &str, repo_branch: &str, wt_dir: PathBuf, wt_branch: &str) -> Self {
            Self {
                head_sha: head_sha.to_string(),
                branch: repo_branch.to_string(),
                wt_dir: Some(wt_dir),
                wt_branch: Some(wt_branch.to_string()),
            }
        }
    }

    impl GitOps for MockGit {
        fn run_in(&self, _dir: &Path, args: &[&str]) -> std::io::Result<Output> {
            let branch_bytes = if let (Some(wt), Some(wt_b)) = (&self.wt_dir, &self.wt_branch) {
                if _dir == wt {
                    wt_b.as_bytes().to_vec()
                } else {
                    self.branch.as_bytes().to_vec()
                }
            } else {
                self.branch.as_bytes().to_vec()
            };
            let stdout = if args.contains(&"rev-parse") && args.contains(&"HEAD") {
                self.head_sha.as_bytes().to_vec()
            } else if args.contains(&"branch") && args.contains(&"--show-current") {
                branch_bytes
            } else {
                Vec::new()
            };
            Ok(Output {
                status: ExitStatus::from_raw(0),
                stdout,
                stderr: Vec::new(),
            })
        }

        fn run(&self, _args: &[&str]) -> std::io::Result<Output> {
            unimplemented!()
        }

        fn rev_parse_commit(&self, _repo: &Path, _base: &str) -> bool {
            true
        }

        fn worktree_list_porcelain(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
            Ok(Vec::new())
        }

        fn worktree_add(
            &self,
            _repo: &Path,
            _branch: &str,
            _wt: &Path,
            _base: Option<&str>,
        ) -> std::io::Result<Output> {
            unimplemented!()
        }

        fn branch_exists(&self, _repo: &Path, _branch: &str) -> bool {
            true
        }

        fn worktree_add_existing(
            &self,
            _repo: &Path,
            _branch: &str,
            _wt: &Path,
        ) -> std::io::Result<Output> {
            unimplemented!()
        }

        fn worktree_remove(&self, _repo: &Path, _wt: &Path) -> std::io::Result<Output> {
            unimplemented!()
        }

        fn remote_list(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
            Ok(Vec::new())
        }

        fn is_core_bare(&self, _repo: &Path) -> bool {
            false
        }

        fn has_core_worktree(&self, _repo: &Path) -> bool {
            false
        }

        fn enable_worktree_config(&self, _repo: &Path) -> std::io::Result<Output> {
            Ok(Output {
                status: ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }

        fn set_worktree_pushurl_blocked(&self, _wt: &Path, _remote: &str) -> std::io::Result<()> {
            Ok(())
        }

        fn is_git_dir(&self, _cwd: &Path) -> bool {
            true
        }

        fn diff_and_status(&self, _cwd: &Path) -> (String, String) {
            (String::new(), String::new())
        }
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("quaqua-verdict-{name}-{nanos}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // 1. reviewer Verdict: clean + baseline -> review-clean at the baseline SHA
    #[test]
    fn test_verdict_acceptance_case1_reviewer_clean_at_baseline_sha() {
        let tmp = TempDir::new("case1");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let name = "suhuynh-task-r1";
        WorkerMetadata::mark_review(&runs_dir, name).unwrap();

        let wt = tmp.path().join(".wt").join(name);
        fs::create_dir_all(&wt).unwrap();
        let baseline_sha = "1111222233334444555566667777888899990000";
        fs::write(
            runs_dir.join(format!(".{name}.readonly")),
            format!("{}\nHEAD {baseline_sha}\n", wt.display()),
        )
        .unwrap();

        let report_content = "# Review\nVerdict: clean\n## Lessons\nnone\n";
        let git = MockGit::new(baseline_sha, "quaqua/suhuynh-task-r1");

        let res = record_verdict_on_acceptance(&runs_dir, name, report_content, true, &git, None);
        assert!(res.is_ok(), "acceptance should succeed");
        let verdict = res.unwrap().expect("verdict should be recorded");

        assert_eq!(verdict.level, VerdictLevel::ReviewClean);
        assert_eq!(verdict.sha, baseline_sha);
        assert_eq!(verdict.source, VerdictSource::Verifier);
        assert_eq!(verdict.verifier, name);
        assert_eq!(verdict.verifier_kind, Some("suhuynh".to_string()));

        let journal = SqliteJournal::open(&runs_dir.join("quaqua.db")).unwrap();
        let loaded = journal
            .strongest_verdict(&verdict.branch, baseline_sha)
            .unwrap();
        assert_eq!(loaded, Some(verdict));
    }

    // 2. builder SHA != worktree HEAD -> inconclusive
    #[test]
    fn test_verdict_acceptance_case2_builder_sha_differs_from_head_yields_inconclusive() {
        let tmp = TempDir::new("case2");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let name = "sude-task";
        let wt = tmp.path().join(".wt").join(name);
        fs::create_dir_all(&wt).unwrap();

        let worktree_head = "2222222222222222222222222222222222222222";
        let reported_sha = "1111111111111111111111111111111111111111";

        let report_content =
            format!("# Build\nVerified: unit-test-verified {reported_sha}\n## Lessons\nnone\n");
        let git = MockGit::new(worktree_head, "quaqua/sude-task");

        let res = record_verdict_on_acceptance(
            &runs_dir,
            name,
            &report_content,
            false,
            &git,
            Some(tmp.path()),
        );
        assert!(res.is_ok(), "acceptance should succeed");
        let verdict = res.unwrap().expect("verdict should be recorded");

        assert_eq!(verdict.level, VerdictLevel::Inconclusive);
        assert_eq!(verdict.sha, worktree_head);
        assert_eq!(verdict.source, VerdictSource::SelfReport);
        assert_eq!(verdict.builder_kind, Some("sude".to_string()));

        let journal = SqliteJournal::open(&runs_dir.join("quaqua.db")).unwrap();
        let loaded = journal
            .strongest_verdict(&verdict.branch, worktree_head)
            .unwrap();
        assert_eq!(loaded.map(|v| v.level), Some(VerdictLevel::Inconclusive));
    }

    // 3. reviewer overrides self-report on the same SHA
    #[test]
    fn test_verdict_acceptance_case3_reviewer_overrides_self_report_on_same_sha() {
        let tmp = TempDir::new("case3");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let task = "pstack-test";
        let sha = "aaaa1111bbbb2222cccc3333dddd4444eeee5555";
        let branch = "quaqua/sude-pstack-test";

        // 1. Builder reports self-verdict
        let builder_name = "sude-pstack-test";
        let b_wt = tmp.path().join(".wt").join(builder_name);
        fs::create_dir_all(&b_wt).unwrap();
        let b_report = format!("# Build\nVerified: unit-test-verified {sha}\n## Lessons\nnone\n");
        let b_git = MockGit::new(sha, branch);
        record_verdict_on_acceptance(
            &runs_dir,
            builder_name,
            &b_report,
            false,
            &b_git,
            Some(tmp.path()),
        )
        .unwrap();

        // 2. Reviewer reports clean verdict on the same branch + SHA
        let rev_name = "suhuynh-pstack-test-r1";
        WorkerMetadata::mark_review(&runs_dir, rev_name).unwrap();
        WorkerMetadata::save_pipeline(&runs_dir, rev_name, task).unwrap();
        fs::write(
            runs_dir.join(format!("pipeline-{task}.state")),
            "builder=sude\nreviewer=suhuynh\n",
        )
        .unwrap();

        let r_wt = tmp.path().join(".wt").join(rev_name);
        fs::create_dir_all(&r_wt).unwrap();
        fs::write(
            runs_dir.join(format!(".{rev_name}.readonly")),
            format!("{}\nHEAD {sha}\n", r_wt.display()),
        )
        .unwrap();

        let r_report = "# Review\nVerdict: clean\n## Lessons\nnone\n";
        let r_git = MockGit::new(sha, branch);
        record_verdict_on_acceptance(
            &runs_dir,
            rev_name,
            r_report,
            true,
            &r_git,
            Some(tmp.path()),
        )
        .unwrap();

        // 3. Verify strongest verdict is the reviewer verdict
        let journal = SqliteJournal::open(&runs_dir.join("quaqua.db")).unwrap();
        let strongest = journal
            .strongest_verdict(branch, sha)
            .unwrap()
            .expect("must find verdict");

        assert_eq!(strongest.level, VerdictLevel::ReviewClean);
        assert_eq!(strongest.source, VerdictSource::Verifier);
        assert_eq!(strongest.verifier, rev_name);
        assert_eq!(strongest.builder_kind, Some("sude".to_string()));
        assert_eq!(strongest.verifier_kind, Some("suhuynh".to_string()));

        // 4. Also verify reviewer with findings overrides self-report
        let rev2_name = "truonglao-pstack-test-r2";
        WorkerMetadata::mark_review(&runs_dir, rev2_name).unwrap();
        WorkerMetadata::save_pipeline(&runs_dir, rev2_name, task).unwrap();
        let r2_wt = tmp.path().join(".wt").join(rev2_name);
        fs::create_dir_all(&r2_wt).unwrap();
        fs::write(
            runs_dir.join(format!(".{rev2_name}.readonly")),
            format!("{}\nHEAD {sha}\n", r2_wt.display()),
        )
        .unwrap();
        let r2_report = "# Review\nVerdict: 2 findings\n## Lessons\nnone\n";
        let r2_git = MockGit::new(sha, branch);
        let r2_res = record_verdict_on_acceptance(
            &runs_dir,
            rev2_name,
            r2_report,
            true,
            &r2_git,
            Some(tmp.path()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r2_res.level, VerdictLevel::VerifierFailed);
        let self_verdict = VerdictRecord {
            branch: branch.to_string(),
            sha: sha.to_string(),
            level: VerdictLevel::UnitTestVerified,
            evidence: String::new(),
            verifier: builder_name.to_string(),
            source: VerdictSource::SelfReport,
            builder_kind: Some("sude".to_string()),
            verifier_kind: None,
            harness: String::new(),
        };
        assert!(
            r2_res.strength() > self_verdict.strength(),
            "verifier-failed must override self-report unit-test-verified"
        );
    }

    // 4. double resume -> one verdict per key
    #[test]
    fn test_verdict_acceptance_case4_double_resume_yields_one_verdict_per_key() {
        let tmp = TempDir::new("case4");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let name = "sude-task";
        let wt = tmp.path().join(".wt").join(name);
        fs::create_dir_all(&wt).unwrap();
        let sha = "1111111111111111111111111111111111111111";
        let report_content =
            format!("# Build\nVerified: unit-test-verified {sha}\n## Lessons\nnone\n");
        let git = MockGit::new(sha, "quaqua/sude-task");

        // First acceptance
        let res1 = record_verdict_on_acceptance(
            &runs_dir,
            name,
            &report_content,
            false,
            &git,
            Some(tmp.path()),
        );
        assert!(res1.is_ok());

        // Second acceptance (double resume)
        let res2 = record_verdict_on_acceptance(
            &runs_dir,
            name,
            &report_content,
            false,
            &git,
            Some(tmp.path()),
        );
        assert!(res2.is_ok());

        let journal = SqliteJournal::open(&runs_dir.join("quaqua.db")).unwrap();
        let list = journal.load_verdicts("quaqua/sude-task", sha).unwrap();
        assert_eq!(
            list.len(),
            1,
            "double resume must yield exactly one verdict per key"
        );
    }

    // 5. locked or missing db does not fail acceptance
    #[test]
    fn test_verdict_acceptance_case5_locked_or_missing_db_does_not_fail_acceptance() {
        let tmp = TempDir::new("case5");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        // Create a directory named quaqua.db where a file is expected, causing SQLite open to fail
        let bad_db = runs_dir.join("quaqua.db");
        fs::create_dir_all(&bad_db).unwrap();

        let name = "sude-task";
        let wt = tmp.path().join(".wt").join(name);
        fs::create_dir_all(&wt).unwrap();
        let sha = "1111111111111111111111111111111111111111";
        let report_content =
            format!("# Build\nVerified: unit-test-verified {sha}\n## Lessons\nnone\n");
        let git = MockGit::new(sha, "quaqua/sude-task");

        let res = record_verdict_on_acceptance(
            &runs_dir,
            name,
            &report_content,
            false,
            &git,
            Some(tmp.path()),
        );
        assert!(res.is_ok(), "locked or missing db must not fail acceptance");
        assert!(res.unwrap().is_some());
    }

    // 6. unknown level -> inconclusive
    #[test]
    fn test_verdict_acceptance_case6_unknown_level_yields_inconclusive() {
        let tmp = TempDir::new("case6");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let name = "sude-task";
        let wt = tmp.path().join(".wt").join(name);
        fs::create_dir_all(&wt).unwrap();
        let sha = "3333333333333333333333333333333333333333";
        let report_content =
            format!("# Build\nVerified: mysterious-super-pass {sha}\n## Lessons\nnone\n");
        let git = MockGit::new(sha, "quaqua/sude-task");

        let res = record_verdict_on_acceptance(
            &runs_dir,
            name,
            &report_content,
            false,
            &git,
            Some(tmp.path()),
        );
        assert!(res.is_ok());
        let verdict = res.unwrap().expect("verdict recorded");
        assert_eq!(verdict.level, VerdictLevel::Inconclusive);

        // The level is not silently swallowed: accept warns, names the offending
        // token and quotes the six valid levels, and never blocks.
        let mut warnings: Vec<String> = Vec::new();
        let res = record_verdict_on_acceptance_warned(
            &runs_dir,
            name,
            &report_content,
            false,
            &git,
            Some(tmp.path()),
            &mut |msg: &str| warnings.push(msg.to_string()),
        );
        assert!(res.is_ok(), "a bad level must not fail acceptance");
        assert_eq!(
            warnings.len(),
            1,
            "an unrecognised level warns once at accept, got {warnings:?}"
        );
        assert!(
            warnings[0].contains("mysterious-super-pass"),
            "warning must name the offending level: {}",
            warnings[0]
        );
        assert!(
            warnings[0].contains(&VerdictLevel::vocabulary()),
            "warning must name the six valid levels: {}",
            warnings[0]
        );

        // A recognised level is quiet, so the check above can see a warning at all.
        let ok_name = "sude-task2";
        let ok_sha = "4444444444444444444444444444444444444444";
        fs::create_dir_all(tmp.path().join(".wt").join(ok_name)).unwrap();
        let ok_report = format!("# Build\nVerified: type-check-only {ok_sha}\n## Lessons\nnone\n");
        let ok_git = MockGit::new(ok_sha, "quaqua/sude-task2");
        let mut ok_warnings: Vec<String> = Vec::new();
        record_verdict_on_acceptance_warned(
            &runs_dir,
            ok_name,
            &ok_report,
            false,
            &ok_git,
            Some(tmp.path()),
            &mut |msg: &str| ok_warnings.push(msg.to_string()),
        )
        .unwrap();
        assert!(
            ok_warnings.is_empty(),
            "a recognised level must not warn: {ok_warnings:?}"
        );

        // Also verify deserialization of unknown level from JSON
        let json = r#"{"branch":"quaqua/sude-task","sha":"3333333333333333333333333333333333333333","level":"unheard-of-level","evidence":"","verifier":"sude-task","source":"self","harness":"omp"}"#;
        let decoded: VerdictRecord = serde_json::from_str(json).unwrap();
        assert_eq!(decoded.level, VerdictLevel::Inconclusive);
    }

    // 7. the vocabulary has one home: the worker-facing report contract quotes it
    #[test]
    fn test_builder_level_vocabulary_documented_in_report_contract() {
        let brief = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../templates/workspace/rules/_all/brief.md");
        let doc = fs::read_to_string(&brief)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", brief.display()));
        let vocabulary = VerdictLevel::vocabulary();
        assert!(
            doc.contains(&vocabulary),
            "rules/_all/brief.md `## Report format` must list the levels as {vocabulary} — \
             a builder has nowhere else to learn them"
        );
    }

    #[test]
    fn test_f1_standalone_reviewer_with_base_records_on_builder_branch_with_kind() {
        let tmp = TempDir::new("f1");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let reviewer_name = "suhuynh-pstackverdr";
        let builder_branch = "quaqua/sude-pstackverdict";
        let builder_kind = "sude";
        let sha = "c63689f8dabcbacaf01a9637c436b030a22b9126";

        WorkerMetadata::mark_review(&runs_dir, reviewer_name).unwrap();
        fs::write(
            runs_dir.join(format!("{reviewer_name}.base")),
            format!("{builder_branch}\n"),
        )
        .unwrap();

        let r_wt = tmp.path().join(".wt").join(reviewer_name);
        fs::create_dir_all(&r_wt).unwrap();
        fs::write(
            runs_dir.join(format!(".{reviewer_name}.readonly")),
            format!("{}\nHEAD {sha}\n", r_wt.display()),
        )
        .unwrap();

        let r_report = "# Review\nVerdict: clean\n## Lessons\nnone\n";
        let git = MockGit::new(sha, "quaqua/suhuynh-pstackverdr");

        let res = record_verdict_on_acceptance(
            &runs_dir,
            reviewer_name,
            r_report,
            true,
            &git,
            Some(tmp.path()),
        );
        let verdict = res.unwrap().expect("must record verdict");
        assert_eq!(verdict.branch, builder_branch);
        assert_eq!(verdict.builder_kind.as_deref(), Some(builder_kind));

        let journal = SqliteJournal::open(&runs_dir.join("quaqua.db")).unwrap();
        let loaded = journal.strongest_verdict(builder_branch, sha).unwrap();
        assert!(
            loaded.is_some(),
            "lookup for builder branch at that SHA must find it"
        );
        assert_eq!(loaded.unwrap().builder_kind.as_deref(), Some(builder_kind));
    }

    #[test]
    fn test_f3_reviewer_verdict_parser_matches_pipeline_strictness() {
        let (lvl_0, _) = parse_reviewer_report("# Report\nVerdict: 0 findings\n## Lessons\nnone\n");
        assert_eq!(
            lvl_0,
            VerdictLevel::ReviewClean,
            "0 findings must be parsed as clean"
        );

        let (lvl_dot, _) = parse_reviewer_report("# Report\nVerdict: clean.\n## Lessons\nnone\n");
        assert_eq!(
            lvl_dot,
            VerdictLevel::Inconclusive,
            "clean. with trailing dot must fail closed as inconclusive"
        );

        let (lvl_trailing, _) =
            parse_reviewer_report("# Report\nVerdict: clean but see finding 2\n## Lessons\nnone\n");
        assert_eq!(
            lvl_trailing,
            VerdictLevel::Inconclusive,
            "clean with trailing commentary must fail closed"
        );
    }

    #[test]
    fn test_f4_two_verifier_verdicts_negative_dominates() {
        let tmp = TempDir::new("f4");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();
        let mut journal = SqliteJournal::open(&runs_dir.join("quaqua.db")).unwrap();

        let branch = "quaqua/sude-task";
        let sha = "abcd1234abcd1234abcd1234abcd1234abcd1234";

        let v1_clean = VerdictRecord {
            branch: branch.to_string(),
            sha: sha.to_string(),
            level: VerdictLevel::ReviewClean,
            evidence: "clean".to_string(),
            verifier: "suhuynh-r1".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("suhuynh".to_string()),
            harness: "opencode".to_string(),
        };
        let v2_failed = VerdictRecord {
            branch: branch.to_string(),
            sha: sha.to_string(),
            level: VerdictLevel::VerifierFailed,
            evidence: "2 findings".to_string(),
            verifier: "truonglao-r2".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("truonglao".to_string()),
            harness: "opencode".to_string(),
        };

        journal.record_verdict("wf-v1", &v1_clean).unwrap();
        journal.record_verdict("wf-v2", &v2_failed).unwrap();

        let strongest = journal.strongest_verdict(branch, sha).unwrap().unwrap();
        assert_eq!(strongest.level, VerdictLevel::VerifierFailed);
        assert_ne!(strongest.level, VerdictLevel::ReviewClean);
    }

    #[test]
    fn test_f5_reviewer_without_resolvable_sha_never_passes() {
        let tmp = TempDir::new("f5");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let rev_name = "suhuynh-nosha";
        WorkerMetadata::mark_review(&runs_dir, rev_name).unwrap();
        let report = "# Review\nVerdict: clean\n## Lessons\nnone\n";
        let git = MockGit::new("", "quaqua/suhuynh-nosha");

        let res = record_verdict_on_acceptance(&runs_dir, rev_name, report, true, &git, None);
        let verdict = res.unwrap().expect("must return verdict record");
        assert_eq!(verdict.sha, "");
        assert_ne!(verdict.level, VerdictLevel::ReviewClean);
        assert_eq!(verdict.level, VerdictLevel::Inconclusive);
    }

    #[test]
    fn test_f6_query_cannot_present_self_report_alone_as_verified() {
        let tmp = TempDir::new("f6");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();
        let mut journal = SqliteJournal::open(&runs_dir.join("quaqua.db")).unwrap();

        let branch = "quaqua/sude-task";
        let sha = "abcd1234abcd1234abcd1234abcd1234abcd1234";

        let v_builder = VerdictRecord {
            branch: branch.to_string(),
            sha: sha.to_string(),
            level: VerdictLevel::UnitTestVerified,
            evidence: "tests passed".to_string(),
            verifier: "sude-task".to_string(),
            source: VerdictSource::SelfReport,
            builder_kind: Some("sude".to_string()),
            verifier_kind: None,
            harness: "omp".to_string(),
        };
        journal.record_verdict("wf-builder", &v_builder).unwrap();

        let cur = journal.strongest_verdict(branch, sha).unwrap().unwrap();
        assert_ne!(
            cur.level,
            VerdictLevel::UnitTestVerified,
            "self-report alone cannot be presented as unit-test-verified"
        );
        assert_eq!(cur.level, VerdictLevel::Inconclusive);
        assert!(!cur.is_verified());
    }

    #[test]
    fn test_taskforce_reviewer_does_not_record_branch_verdict() {
        let tmp = TempDir::new("tf-verdict");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let tf_name = "suhuynh-advisor-task";
        WorkerMetadata::mark_review(&runs_dir, tf_name).unwrap();
        fs::write(
            runs_dir.join(format!("{tf_name}.taskforce")),
            "advisor-task\n",
        )
        .unwrap();

        let r_report = "# Report\nVerdict: clean\n## Lessons\nnone\n";
        let git = MockGit::new("1234567890", "quaqua/suhuynh-advisor-task");

        let res = record_verdict_on_acceptance(
            &runs_dir,
            tf_name,
            r_report,
            true,
            &git,
            Some(tmp.path()),
        );
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            None,
            "taskforce workers must not record branch verdicts"
        );
    }

    #[test]
    fn test_finding1_repo_basename_matches_worker_name_resolves_wt_branch() {
        let tmp = TempDir::new("f1-basename");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let name = "sude-task";
        let repo_dir = tmp.path().join(name);
        let wt_dir = tmp.path().join(".wt").join(name);
        fs::create_dir_all(&repo_dir).unwrap();
        fs::create_dir_all(&wt_dir).unwrap();

        // 1. resolve_worker_worktree must pick the worktree, not repo_dir
        let wt = resolve_worker_worktree(&runs_dir, name, Some(&repo_dir));
        assert_eq!(
            wt,
            Some(wt_dir.clone()),
            "must resolve worktree at .wt/<name>, not repo_dir checkout"
        );

        let git = MockGit::with_wt(
            "1111111111111111111111111111111111111111",
            "main",
            wt_dir.clone(),
            "quaqua/sude-task",
        );
        let report = "# Build\nVerified: unit-test-verified 1111111111111111111111111111111111111111\n## Lessons\nnone\n";
        let rec =
            record_verdict_on_acceptance(&runs_dir, name, report, false, &git, Some(&repo_dir))
                .unwrap()
                .expect("record returned");
        assert_eq!(
            rec.branch, "quaqua/sude-task",
            "must record on worker branch, not main checkout branch"
        );
    }

    #[test]
    fn test_finding2_unparseable_reviewer_verdict_not_recorded() {
        let tmp = TempDir::new("f2-verdict");
        let runs_dir = tmp.path().join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let rev_name = "suhuynh-taskr1";
        WorkerMetadata::mark_review(&runs_dir, rev_name).unwrap();
        let wt = tmp.path().join(".wt").join(rev_name);
        fs::create_dir_all(&wt).unwrap();
        let sha = "1111111111111111111111111111111111111111";
        fs::write(
            readonly::baseline_path(&runs_dir, rev_name),
            format!("{}\n{sha}\n", wt.display()),
        )
        .unwrap();

        let git = MockGit::new(sha, "quaqua/suhuynh-taskr1");

        // 1. Report with unparseable verdict line (e.g. "Verdict: not clean")
        let bad_report = "# Review\nVerdict: not clean\n## Lessons\nnone\n";
        let res_bad = record_verdict_on_acceptance(
            &runs_dir,
            rev_name,
            bad_report,
            true,
            &git,
            Some(tmp.path()),
        );
        assert!(res_bad.is_ok());
        assert_eq!(
            res_bad.unwrap(),
            None,
            "unparseable reviewer report must return None and NOT record a verdict"
        );

        // Ensure database has no records
        let db_path = runs_dir.join("quaqua.db");
        if let Ok(journal) = SqliteJournal::open(&db_path) {
            let records = journal.load_verdicts("quaqua/suhuynh-taskr1", sha).unwrap();
            assert!(
                records.is_empty(),
                "rejected report must not leave an inconclusive verdict in DB"
            );
        }

        // 2. Report is corrected to clean -> successfully recorded as ReviewClean
        let clean_report = "# Review\nVerdict: clean\n## Lessons\nnone\n";
        let res_clean = record_verdict_on_acceptance(
            &runs_dir,
            rev_name,
            clean_report,
            true,
            &git,
            Some(tmp.path()),
        );
        assert!(res_clean.is_ok());
        let rec = res_clean.unwrap().expect("must record clean verdict");
        assert_eq!(rec.level, VerdictLevel::ReviewClean);

        let journal = SqliteJournal::open(&db_path).unwrap();
        let strongest = journal
            .strongest_verdict("quaqua/suhuynh-taskr1", sha)
            .unwrap()
            .unwrap();
        assert_eq!(strongest.level, VerdictLevel::ReviewClean);
    }
}
