//! `quaqua verify record`: a coordinator records its own test rerun as a
//! verifier verdict for a head SHA, so `status --metrics` G1 counts the merge
//! of that head as verified (G1 matches journal verdicts by head SHA).

use std::path::{Path, PathBuf};

use crate::engine::journal_sqlite::{SqliteJournal, VerdictLevel, VerdictRecord, VerdictSource};
use crate::git::{Git, GitOps};

pub const USAGE: &str = "usage: quaqua verify record <worker|branch|sha> --cmd \"<test cmd>\" --level <level> [--repo <path>]";

/// What a `record_rerun` call left in the journal.
#[derive(Debug, PartialEq, Eq)]
pub struct Recorded {
    pub branch: String,
    pub sha: String,
    /// False when the same rerun was already recorded (re-running is a no-op).
    pub inserted: bool,
}

/// Resolves `target` (sha, ref, or worker name -> `quaqua/<name>`) to a full
/// commit SHA and the branch label the verdict is filed under.
fn resolve_target<G: GitOps>(
    git: &G,
    repo: &Path,
    target: &str,
) -> Result<(String, String), String> {
    let candidates = [target.to_string(), format!("quaqua/{target}")];
    for cand in &candidates {
        let spec = format!("{cand}^{{commit}}");
        let out = git
            .run_in(repo, &["rev-parse", "--verify", "--quiet", &spec])
            .map_err(|e| format!("error: git failed: {e}"))?;
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return Ok((cand.clone(), sha));
        }
    }
    Err(format!(
        "error: '{target}' is not a commit, branch, or worker in {}",
        repo.display()
    ))
}

/// Appends a verifier verdict for the head `target` resolves to.
///
/// Idempotent: the journal dedupes on (branch, sha, source, verifier), and the
/// verifier name carries the level, so repeating a rerun adds nothing while a
/// later rerun at a different level is kept (a failed rerun still dominates a
/// pass at the same head, per the S1 rules).
pub fn record_rerun<G: GitOps>(
    db_path: &Path,
    repo: &Path,
    target: &str,
    cmd: &str,
    level: VerdictLevel,
    git: &G,
) -> Result<Recorded, String> {
    let (branch, sha) = resolve_target(git, repo, target)?;
    let verdict = VerdictRecord {
        branch: branch.clone(),
        sha: sha.clone(),
        level,
        evidence: format!("coordinator rerun: {cmd}"),
        verifier: format!("rerun:{}", level.as_str()),
        source: VerdictSource::Verifier,
        builder_kind: None,
        verifier_kind: None,
        harness: String::new(),
    };
    let mut journal = SqliteJournal::open(db_path)
        .map_err(|e| format!("error: journal unavailable at {}: {e}", db_path.display()))?;
    let wf_id = format!("rerun:{}", &sha[..sha.len().min(12)]);
    let inserted = journal
        .record_verdict(&wf_id, &verdict)
        .map_err(|e| format!("error: cannot record verdict: {e}"))?;
    Ok(Recorded {
        branch,
        sha,
        inserted,
    })
}

fn flag_value(args: &[String], flag: &str) -> Result<Option<String>, String> {
    match args.iter().position(|a| a == flag) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .filter(|v| !v.starts_with("--"))
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("error: {flag} needs a value\n{USAGE}")),
    }
}

/// CLI entrypoint for `quaqua verify`.
pub fn handle_verify(args: &[String]) -> Result<i32, String> {
    if args.first().map(String::as_str) != Some("record") {
        return Err(USAGE.to_string());
    }
    let rest = &args[1..];
    let target = rest
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or_else(|| USAGE.to_string())?;
    let cmd =
        flag_value(rest, "--cmd")?.ok_or_else(|| format!("error: --cmd is required\n{USAGE}"))?;
    let level_tok = flag_value(rest, "--level")?
        .ok_or_else(|| format!("error: --level is required\n{USAGE}"))?;
    let level = VerdictLevel::parse_known(&level_tok).ok_or_else(|| {
        format!(
            "error: unknown level '{level_tok}'; use one of {}",
            VerdictLevel::vocabulary()
        )
    })?;
    let repo = match flag_value(rest, "--repo")? {
        Some(r) => PathBuf::from(r),
        None => std::env::current_dir().map_err(|e| format!("error: no cwd: {e}"))?,
    };
    let runs_dir = if let Ok(dir) = std::env::var("QUAQUA_RUNS_DIR") {
        PathBuf::from(dir)
    } else {
        crate::workspace::workspace_root().join("runs")
    };
    let rec = record_rerun(
        &runs_dir.join("quaqua.db"),
        &repo,
        target,
        &cmd,
        level,
        &Git::new(),
    )?;
    println!(
        "{} verdict {} for {} @ {}",
        if rec.inserted {
            "recorded"
        } else {
            "already recorded:"
        },
        level.as_str(),
        rec.branch,
        rec.sha
    );
    Ok(0)
}
