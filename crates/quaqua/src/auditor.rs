//! quaqua auditor — read-only drift and pipeline audit.
//!
//! Replaces the legacy auditor shell script with exact behavioral parity.
//! Runs deterministic pre-checks over pipeline runs, challenges, live workers,
//! and instruction docs, builds the audit prompt, executes the headless harness
//! (`opencode` or `claude`), and writes the final report to `reports/audit-YYYY-MM-DD-HH.md`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const USAGE: &str = "# Usage: quaqua auditor run [--dry-run] [--out <path>] [--model <model>] [--harness <harness>] [--repo <path>]";

pub const PROMPT_TEMPLATE: &str = r#"You are the Auditor for the quaqua repo. READ-ONLY: inspect and report only.
Do not edit, create, or delete any file; do not run mutating commands; do not
contact, prompt, or instruct any agent. If a redirect is needed, flag it in the
report — the CoS routes it.

Audit six things:
1. Drift in CoS activity. Read memory/ files from the last 2 days, projects/*.md,
   and `git log` (last ~30 commits) of each repo listed in projects/INDEX.md.
   Flag: claimed outcomes the code or git history does not back; project
   "Now"/"Log" entries contradicted by repo state; memory rules or lessons that
   rest on stale facts.
2. Stale docs vs real code/scripts. Flag: AGENTS.md, memory/rules.md, models.md,
   rules/ entries that reference removed scripts/behaviors or describe a flow the
   scripts no longer implement; scripts or docs/plans that contradict each other;
   instruction docs over 100 lines (doc-rot: agents trust them over the code).
3. Unhandled memory. Flag: reports/audit-*.md with no
   `- [audit] <report file>:` line in any memory/YYYY-MM-DD.md (the
   handled marker CoS writes, see AGENTS.md); `## Lessons` lines (other than
   `none`) in runs/*.md from the last 7 days that no rules/ file cites as
   `runs/<name>.md`.
4. Pipeline runs (runs/pipeline-*.md, written by `quaqua pipeline`; the
   last `- status:` line is the outcome). Flag:
   - `status: disputed|blocked` with no memory/YYYY-MM-DD.md line naming
     `pipeline-<task>` (the CoS handling note, same convention as the
     `[audit]` report marker);
   - `status: clean` whose `commit` is `none`/`?` or that
     `git rev-parse --verify <sha>^{commit}` cannot resolve in the pipeline's
     recorded repo (the `- repo:` header in runs/pipeline-<task>.md — Peers
     build in project repos, not this checkout; a missing recorded repo is a
     separate flag);
   - `status: clean` where review ran (`- review round <N>:` lines — not
     `review: skipped`) but neither the last review-round line nor the
     reviewer runs file it names carries a `Verdict:` line;
   - a `## Challenge` section in any runs/*.md (except *.rules.md — injected
     rule dumps where the word describes a rule, not a challenge) with no
     memory line naming the worker. Challenges are resolved by the Lead
     before acceptance; no handling line = still open. A Challenge already
     copied into a pipeline file's `## Challenge (from <file>)` section is
     covered by that file's flag — do not report it twice.
5. Live workers. Flag any live task worker in herdr that is stalled, untracked,
   or in an unhandled error state.
6. Lead inbox timeline review. Inspect Lead-to-CoS inbox
   timelines for authority-gradient, self-acceptance, and lost-momentum with
   file:line. Flag:
   - unverified acceptances or handoffs citing missing runs/<x>.md or raw/<x>.md files;
   - self-acceptance or authority-gradient where an accept cites runs files with
     Outcome: BLOCKED or DISPUTED;
   - lost-momentum: a Lead whose last inbox entry is older than 24h while still
     owning live workers.
   Deterministic pre-check — the script already ran the cheap greps; list
   each finding below under `## Pipeline`, `## Workers` or (doc-rot)
   `## Stale docs` unless repo evidence
   contradicts it, and add any flag the greps missed:
__PRECHECK__

Write your whole report as the final message — it becomes the report file.
Format: `## Drift`, `## Stale docs`, `## Unhandled`, `## Pipeline`, and `## Workers`
sections, each `- file: finding + evidence` (cite path/commit), one line each.
No findings in a section → write `- none`. End with one line
`Verdict: clean | N findings`. Terse, technical."#;

#[derive(Debug, Default, Clone)]
pub struct AuditorRunOptions {
    pub dry_run: bool,
    pub out: Option<PathBuf>,
    pub model: Option<String>,
    pub harness: Option<String>,
    pub repo: Option<PathBuf>,
}

pub fn handle_auditor(args: &[String]) -> Result<i32, (i32, String)> {
    if args.is_empty() {
        return Err((1, format!("{USAGE}\n")));
    }

    match args[0].as_str() {
        "run" => {
            let mut opts = AuditorRunOptions::default();
            let mut i = 1;
            while i < args.len() {
                let arg = &args[i];
                if arg == "--dry-run" {
                    opts.dry_run = true;
                    i += 1;
                } else if arg == "--out" {
                    if i + 1 < args.len() {
                        opts.out = Some(PathBuf::from(&args[i + 1]));
                        i += 2;
                    } else {
                        return Err((1, format!("error: --out requires a path\n{USAGE}\n")));
                    }
                } else if let Some(val) = arg.strip_prefix("--out=") {
                    opts.out = Some(PathBuf::from(val));
                    i += 1;
                } else if arg == "--model" {
                    if i + 1 < args.len() {
                        opts.model = Some(args[i + 1].clone());
                        i += 2;
                    } else {
                        return Err((1, format!("error: --model requires a value\n{USAGE}\n")));
                    }
                } else if let Some(val) = arg.strip_prefix("--model=") {
                    opts.model = Some(val.to_string());
                    i += 1;
                } else if arg == "--harness" {
                    if i + 1 < args.len() {
                        opts.harness = Some(args[i + 1].clone());
                        i += 2;
                    } else {
                        return Err((1, format!("error: --harness requires a value\n{USAGE}\n")));
                    }
                } else if let Some(val) = arg.strip_prefix("--harness=") {
                    opts.harness = Some(val.to_string());
                    i += 1;
                } else if arg == "--repo" {
                    if i + 1 < args.len() {
                        opts.repo = Some(PathBuf::from(&args[i + 1]));
                        i += 2;
                    } else {
                        return Err((1, format!("error: --repo requires a path\n{USAGE}\n")));
                    }
                } else if let Some(val) = arg.strip_prefix("--repo=") {
                    opts.repo = Some(PathBuf::from(val));
                    i += 1;
                } else if arg == "--help" || arg == "-h" {
                    println!("{USAGE}");
                    return Ok(0);
                } else {
                    return Err((
                        1,
                        format!("error: unknown auditor option '{arg}'\n{USAGE}\n"),
                    ));
                }
            }
            run_auditor(opts)
        }
        "--help" | "-h" => {
            println!("{USAGE}");
            Ok(0)
        }
        unknown => Err((
            1,
            format!("error: unknown auditor command '{unknown}'\n{USAGE}\n"),
        )),
    }
}

pub fn run_auditor(opts: AuditorRunOptions) -> Result<i32, (i32, String)> {
    let root = resolve_auditor_root(opts.repo.as_deref());

    let out = resolve_out_path(&root, opts.out.as_deref());
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| (1, format!("error creating report dir: {e}\n")))?;
    }

    let flags = run_deterministic_prechecks(&root);

    let precheck_block = if flags.is_empty() {
        "- none".to_string()
    } else {
        flags.join("\n")
    };

    let prompt = PROMPT_TEMPLATE.replace("__PRECHECK__", &precheck_block);

    if opts.dry_run {
        let mut report = String::from("## Drift\n- none\n\n## Stale docs\n");
        let stale_flags: Vec<&str> = flags
            .iter()
            .filter(|f| {
                f.contains("doc-rot risk")
                    || f.contains("expired rule")
                    || f.contains("malformed expiry")
            })
            .map(|s| s.as_str())
            .collect();
        if stale_flags.is_empty() {
            report.push_str("- none\n\n");
        } else {
            for f in stale_flags {
                report.push_str(f);
                report.push('\n');
            }
            report.push('\n');
        }

        report.push_str("## Unhandled\n");
        let unhandled_flags: Vec<&str> = flags
            .iter()
            .filter(|f| f.contains("unpromoted") || f.contains("recurring failure"))
            .map(|s| s.as_str())
            .collect();
        if unhandled_flags.is_empty() {
            report.push_str("- none\n\n");
        } else {
            for f in unhandled_flags {
                report.push_str(f);
                report.push('\n');
            }
            report.push('\n');
        }

        report.push_str("## Pipeline\n");
        let pipeline_flags: Vec<&str> = flags
            .iter()
            .filter(|f| {
                f.starts_with("- runs/pipeline-")
                    || (f.starts_with("- runs/") && f.contains("'## Challenge'"))
                    || (f.starts_with("- trigger #") && !f.contains("live worker"))
            })
            .map(|s| s.as_str())
            .collect();
        if pipeline_flags.is_empty() {
            report.push_str("- none\n\n");
        } else {
            for f in pipeline_flags {
                report.push_str(f);
                report.push('\n');
            }
            report.push('\n');
        }

        report.push_str("## Workers\n");
        let worker_flags: Vec<&str> = flags
            .iter()
            .filter(|f| {
                f.contains("herdr") || f.contains("live worker") || f.contains("lost momentum")
            })
            .map(|s| s.as_str())
            .collect();
        if worker_flags.is_empty() {
            report.push_str("- none\n\n");
        } else {
            for f in worker_flags {
                report.push_str(f);
                report.push('\n');
            }
            report.push('\n');
        }

        if flags.is_empty() {
            report.push_str("Verdict: clean\n");
        } else {
            report.push_str(&format!("Verdict: {} findings\n", flags.len()));
        }

        std::fs::write(&out, &report)
            .map_err(|e| (1, format!("error writing dry-run report: {e}\n")))?;
        println!("{}", out.display());
        return Ok(0);
    }

    let harness = resolve_harness(opts.harness.as_deref());
    let (model, mut cmd) = match harness.as_str() {
        "opencode" => {
            let m = opts
                .model
                .clone()
                .or_else(|| {
                    std::env::var("AUDITOR_MODEL")
                        .ok()
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_else(|| "opencode-go/deepseek-v4.1-flash#low".to_string());
            let mut c = Command::new("opencode");
            c.current_dir(&root);
            c.args(["run", "--standalone", "--auto", "-m", &m, &prompt]);
            (m, c)
        }
        "agy" => {
            let m = opts
                .model
                .clone()
                .or_else(|| {
                    std::env::var("AUDITOR_MODEL")
                        .ok()
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_else(|| "gemini-3.8-flash".to_string());
            let mut c = Command::new("agy");
            c.current_dir(&root);
            // `-p` takes the next argument as its prompt, so the prompt is last.
            c.arg("--model")
                .arg(&m)
                .arg("--effort")
                .arg("high")
                .arg("--dangerously-skip-permissions")
                .arg("--output-format")
                .arg("text")
                .arg("-p")
                .arg(&prompt);
            (m, c)
        }
        _ => {
            let m = opts
                .model
                .clone()
                .or_else(|| {
                    std::env::var("AUDITOR_MODEL")
                        .ok()
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_else(|| "claude-opus-5-5".to_string());
            let mut c = Command::new("claude");
            c.current_dir(&root);
            c.arg("-p")
                .arg("--model")
                .arg(&m)
                .arg("--permission-mode")
                .arg("dontAsk")
                .arg("--setting-sources")
                .arg("")
                .arg("--allowedTools")
                .arg("Read,Grep,Glob,Bash(git log:*),Bash(git diff:*),Bash(git show:*)")
                .arg("--disallowedTools")
                .arg("Bash(git *--output*)")
                .arg("--")
                .arg(&prompt);
            (m, c)
        }
    };
    let _ = std::fs::File::create(&out);

    let _ = model;

    let res = cmd.output();
    let (rc, stdout, stderr) = match res {
        Ok(output) => (
            output.status.code().unwrap_or(1),
            output.stdout,
            output.stderr,
        ),
        Err(e) => (1, Vec::new(), e.to_string().into_bytes()),
    };

    if rc != 0 {
        if !stderr.is_empty() {
            let _ = std::io::stderr().write_all(&stderr);
        }
        if !stdout.is_empty() {
            let _ = std::fs::write(&out, &stdout);
        }
        let is_empty = match std::fs::metadata(&out) {
            Ok(meta) => meta.len() == 0,
            Err(_) => true,
        };
        if is_empty {
            let fail_msg = format!(
                "# Auditor {} — FAILED\n\n`{}` exited {}; see scheduler stderr logs (journal or Library/Logs/quaqua-auditor.log).\n",
                current_date_minute(), harness, rc
            );
            let _ = std::fs::write(&out, fail_msg);
        }
        return Err((rc, String::new()));
    }

    std::fs::write(&out, &stdout).map_err(|e| (1, format!("error writing report: {e}\n")))?;
    println!("{}", out.display());
    Ok(0)
}

fn resolve_auditor_root(explicit: Option<&Path>) -> PathBuf {
    resolve_auditor_root_from(
        explicit,
        crate::workspace::env_path("AUDITOR_REPO").as_deref(),
        crate::workspace::env_path("QUAQUA_HOME").as_deref(),
        crate::workspace::env_path("QUAQUA_ROOT").as_deref(),
        crate::workspace::env_path("HOME").as_deref(),
    )
}

/// `--repo` first, then `AUDITOR_REPO`, then the one workspace root
/// (`QUAQUA_HOME` > `QUAQUA_ROOT` > `$HOME/quaqua`). Pure — testable without
/// `set_var`.
fn resolve_auditor_root_from(
    explicit: Option<&Path>,
    auditor_repo: Option<&Path>,
    quaqua_home: Option<&Path>,
    quaqua_root: Option<&Path>,
    home: Option<&Path>,
) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(p) = auditor_repo {
        return p.to_path_buf();
    }
    crate::workspace::workspace_root_from_env(quaqua_home, quaqua_root, home)
}

fn resolve_out_path(root: &Path, explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(out) = std::env::var("AUDITOR_OUT").or_else(|_| std::env::var("SUPERVISOR_OUT")) {
        if !out.is_empty() {
            return PathBuf::from(out);
        }
    }
    root.join("reports")
        .join(format!("audit-{}.md", current_date_hour()))
}

fn resolve_harness(explicit: Option<&str>) -> String {
    if let Some(h) = explicit {
        if !h.is_empty() {
            return h.to_string();
        }
    }
    if let Ok(h) = std::env::var("AUDITOR_HARNESS") {
        if !h.is_empty() {
            return h;
        }
    }
    if is_on_path("claude") {
        "claude".to_string()
    } else if is_on_path("opencode") {
        "opencode".to_string()
    } else if is_on_path("agy") {
        "agy".to_string()
    } else {
        "claude".to_string()
    }
}

pub fn is_on_path(binary: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(binary);
            if candidate.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = candidate.metadata() {
                        if meta.permissions().mode() & 0o111 != 0 {
                            return true;
                        }
                    }
                }
                #[cfg(not(unix))]
                return true;
            }
        }
    }
    false
}

pub fn current_date_hour() -> String {
    if let Ok(output) = Command::new("date").arg("+%Y-%m-%d-%H").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_epoch_secs_date_hour(secs)
}

pub fn current_date_minute() -> String {
    if let Ok(output) = Command::new("date").arg("+%Y-%m-%d %H:%M").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_epoch_secs_date_minute(secs)
}

fn format_epoch_secs_date_hour(secs: u64) -> String {
    let days = secs / 86400;
    let rem = secs % 86400;
    let hour = rem / 3600;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}-{hour:02}")
}

fn format_epoch_secs_date_minute(secs: u64) -> String {
    let days = secs / 86400;
    let rem = secs % 86400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let (y, m, d) = crate::usage::civil_from_days(days as i64);
    (y as u64, m as u64, d as u64)
}

fn ymd_to_days(year: u64, month: u64, day: u64) -> u64 {
    crate::usage::days_from_civil(year as i32, month as u32, day as u32) as u64
}

fn mem_mentions(root: &Path, needles: &[&str]) -> bool {
    let mem_dir = root.join("memory");
    let entries = match std::fs::read_dir(&mem_dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for needle in needles {
                    if content.contains(needle) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn git_verify_commit(repo: &Path, sha: &str) -> bool {
    let arg = format!("{sha}^{{commit}}");
    let res = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", &arg])
        .output();
    matches!(res, Ok(output) if output.status.success())
}

pub fn run_deterministic_prechecks(root: &Path) -> Vec<String> {
    let now_secs = crate::util::now_sec();
    let today = days_to_ymd(now_secs / 86400);
    run_deterministic_prechecks_at(root, today, now_secs)
}

fn parse_ymd(s: &str) -> Option<(u64, u64, u64)> {
    if s.len() != 10 {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = s[0..4].parse().ok()?;
    let m: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let days = crate::usage::days_from_civil(y, m, d);
    if crate::usage::civil_from_days(days) == (y, m, d) {
        Some((y as u64, m as u64, d as u64))
    } else {
        None
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ExpiryStatus {
    ValidExpired,
    Malformed,
}

fn check_line_expiry(line: &str, today: (u64, u64, u64)) -> Option<ExpiryStatus> {
    let trimmed = line.trim_end_matches(['\r', '\n']).trim_end();
    if !trimmed.ends_with(']') {
        return None;
    }
    let idx = trimmed.rfind("[expires:")?;
    let tag = &trimmed[idx..];
    if !tag.ends_with(']') {
        return None;
    }
    let date_str = tag["[expires:".len()..tag.len() - 1].trim();
    match parse_ymd(date_str) {
        Some(exp_ymd) => {
            if exp_ymd < today {
                Some(ExpiryStatus::ValidExpired)
            } else {
                None
            }
        }
        None => Some(ExpiryStatus::Malformed),
    }
}

fn collect_rules_md_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                let path = entry.path();
                if ft.is_dir() {
                    collect_rules_md_files(&path, out);
                } else if ft.is_file()
                    && path.extension().and_then(|ext| ext.to_str()) == Some("md")
                {
                    out.push(path);
                }
            }
        }
    }
}

fn extract_cited_md_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let n = bytes.len();

    let prefixes = ["runs/", "raw/"];
    for prefix in &prefixes {
        let p_bytes = prefix.as_bytes();
        let p_len = p_bytes.len();
        let mut i = 0;
        while i + p_len <= n {
            if &bytes[i..i + p_len] == p_bytes {
                let valid_start = if i == 0 {
                    true
                } else {
                    let prev = bytes[i - 1];
                    !prev.is_ascii_alphanumeric()
                        && prev != b'_'
                        && prev != b'/'
                        && prev != b'.'
                        && prev != b'-'
                };

                if valid_start {
                    let after = &text[i + p_len..];
                    if let Some(md_idx) = after.find(".md") {
                        let path_middle = &after[..md_idx];
                        let valid_middle = !path_middle.is_empty()
                            && path_middle.chars().all(|c| {
                                c.is_ascii_alphanumeric()
                                    || c == '-'
                                    || c == '_'
                                    || c == '.'
                                    || c == '/'
                            });
                        if valid_middle {
                            let candidate = &text[i..i + p_len + md_idx + 3];
                            if !candidate.ends_with(".rules.md") {
                                out.push(candidate.to_string());
                            }
                        }
                    }
                }
            }
            i += 1;
        }
    }

    out.sort();
    out.dedup();
    out
}

pub fn run_deterministic_prechecks_at(
    root: &Path,
    today: (u64, u64, u64),
    now_secs: u64,
) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    // ── 1. pipeline-run pre-checks (runs/pipeline-*.md) ──────────────────────
    let runs_dir = root.join("runs");
    if let Ok(entries) = std::fs::read_dir(&runs_dir) {
        let mut p_files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|name| name.starts_with("pipeline-") && name.ends_with(".md"))
                    .unwrap_or(false)
            })
            .collect();
        p_files.sort();

        for f in p_files {
            let base = f.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let content = match std::fs::read_to_string(&f) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let sline = match content.lines().rfind(|l| l.starts_with("- status: ")) {
                Some(l) => l,
                None => continue,
            };

            let rest = &sline["- status: ".len()..];
            let st_len = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                .count();
            let st = &rest[..st_len];

            match st {
                "disputed" | "blocked" => {
                    if !mem_mentions(root, &[base]) {
                        flags.push(format!("- runs/{base}.md: status {st} but no memory/YYYY-MM-DD.md line names {base}"));
                    }
                }
                "clean" => {
                    let sha = if let Some(idx) = sline.find("commit ") {
                        let after = &sline[idx + "commit ".len()..];
                        after.split_whitespace().next().unwrap_or("").to_string()
                    } else {
                        String::new()
                    };

                    let mut prepo_raw = String::new();
                    for line in content.lines() {
                        if line.starts_with("- task: ") {
                            if let Some(idx) = line.find(" · repo: ") {
                                prepo_raw = line[idx + " · repo: ".len()..].trim().to_string();
                                break;
                            }
                        }
                    }

                    let prepo = if let Some(rest) = prepo_raw.strip_prefix("~/") {
                        if let Ok(home) = std::env::var("HOME") {
                            PathBuf::from(home).join(rest)
                        } else {
                            PathBuf::from(&prepo_raw)
                        }
                    } else if prepo_raw == "~" {
                        if let Ok(home) = std::env::var("HOME") {
                            PathBuf::from(home)
                        } else {
                            PathBuf::from("~")
                        }
                    } else if !prepo_raw.is_empty() {
                        PathBuf::from(&prepo_raw)
                    } else {
                        root.to_path_buf()
                    };

                    if !prepo.is_dir() {
                        flags.push(format!(
                            "- runs/{base}.md: status clean but recorded repo '{}' is missing",
                            prepo.display()
                        ));
                    } else if sha.is_empty()
                        || sha == "none"
                        || sha == "?"
                        || !git_verify_commit(&prepo, &sha)
                    {
                        let sha_disp = if sha.is_empty() { "none" } else { &sha };
                        flags.push(format!("- runs/{base}.md: status clean but commit '{sha_disp}' does not resolve in recorded repo {}", prepo.display()));
                    }

                    let last_rev = content
                        .lines()
                        .rfind(|l| {
                            if let Some(idx) = l.find("review round ") {
                                let after = &l[idx + "review round ".len()..];
                                let digits =
                                    after.chars().take_while(|c| c.is_ascii_digit()).count();
                                digits > 0 && after[digits..].starts_with(':')
                            } else {
                                false
                            }
                        })
                        .unwrap_or("");

                    if !last_rev.is_empty() && !last_rev.contains("Verdict:") {
                        let mut revfile = String::new();
                        if let Some(open) = last_rev.rfind('(') {
                            if let Some(close) = last_rev[open..].find(')') {
                                let inner = &last_rev[open + 1..open + close];
                                if inner.ends_with(".md") {
                                    revfile = inner.to_string();
                                }
                            }
                        }
                        if revfile.is_empty() && last_rev.contains(" spawned on ") {
                            if let Some(idx) = last_rev.find(" spawned on ") {
                                let prefix = &last_rev[..idx];
                                if let Some(colon) = prefix.rfind(':') {
                                    let name = prefix[colon + 1..].trim();
                                    if !name.is_empty() && !name.contains(' ') {
                                        revfile = format!("{name}.md");
                                    }
                                }
                            }
                        }
                        let has_verdict = if !revfile.is_empty() {
                            let rev_path = root.join("runs").join(&revfile);
                            if let Ok(rev_content) = std::fs::read_to_string(&rev_path) {
                                rev_content
                                    .lines()
                                    .any(|l| l.trim_start().starts_with("Verdict:"))
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if !has_verdict {
                            flags.push(format!("- runs/{base}.md: status clean, review ran, but no 'Verdict:' on the last review-round line or in the reviewer runs file it names"));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ── 2. challenge pre-checks (runs/*.md) ───────────────────────────────────
    if let Ok(entries) = std::fs::read_dir(&runs_dir) {
        let mut r_files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|name| name.ends_with(".md") && !name.ends_with(".rules.md"))
                    .unwrap_or(false)
            })
            .collect();
        r_files.sort();

        for f in r_files {
            let content = match std::fs::read_to_string(&f) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if !content.lines().any(|l| l.starts_with("## Challenge")) {
                continue;
            }
            let base = f.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let from_needle = format!("(from {base}.md)");
            let mut copied = false;
            if let Ok(p_entries) = std::fs::read_dir(&runs_dir) {
                for pe in p_entries.flatten() {
                    let p = pe.path();
                    if p != f {
                        if let Some(pname) = p.file_name().and_then(|n| n.to_str()) {
                            if pname.starts_with("pipeline-") && pname.ends_with(".md") {
                                if let Ok(pcontent) = std::fs::read_to_string(&p) {
                                    if pcontent.contains(&from_needle) {
                                        copied = true;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if copied {
                continue;
            }

            let mut short = base.to_string();
            if let Some(s) = short.strip_suffix(".plan") {
                short = s.to_string();
            } else if let Some(s) = short.strip_suffix(".build") {
                short = s.to_string();
            } else if let Some(idx) = short.rfind(".fix") {
                let suffix = &short[idx + ".fix".len()..];
                if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                    short = short[..idx].to_string();
                }
            }

            if short == base {
                if !mem_mentions(root, &[base]) {
                    flags.push(format!("- runs/{base}.md: '## Challenge' unresolved — no memory/YYYY-MM-DD.md line names {base}"));
                }
            } else if !mem_mentions(root, &[base, &short]) {
                flags.push(format!("- runs/{base}.md: '## Challenge' unresolved — no memory/YYYY-MM-DD.md line names {base} or {short}"));
            }
        }
    }

    // ── 3. doc-rot pre-checks ─────────────────────────────────────────────────
    let mut doc_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() && p.extension().and_then(|ext| ext.to_str()) == Some("md") {
                doc_files.push(p);
            }
        }
    }
    let rules_dir = root.join("rules");
    if let Ok(subdirs) = std::fs::read_dir(&rules_dir) {
        for sub in subdirs.flatten() {
            let sub_p = sub.path();
            if sub_p.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&sub_p) {
                    for e in entries.flatten() {
                        let p = e.path();
                        if p.is_file() && p.extension().and_then(|ext| ext.to_str()) == Some("md") {
                            doc_files.push(p);
                        }
                    }
                }
            }
        }
    }
    let projects_dir = root.join("projects");
    if let Ok(entries) = std::fs::read_dir(&projects_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() && p.extension().and_then(|ext| ext.to_str()) == Some("md") {
                doc_files.push(p);
            }
        }
    }
    let mem_rules = root.join("memory").join("rules.md");
    if mem_rules.is_file() {
        doc_files.push(mem_rules.clone());
    }

    doc_files.sort();

    for f in doc_files {
        if let Ok(bytes) = std::fs::read(&f) {
            let n = bytes.iter().filter(|&&b| b == b'\n').count();
            if n > 100 {
                let rel = f.strip_prefix(root).unwrap_or(&f);
                flags.push(format!(
                    "- {}: {} lines (>100) — doc-rot risk, trim or split",
                    rel.display(),
                    n
                ));
            }
        }
    }

    // Collect rules/*.md files once for unpromoted lessons, casebook, and expiry
    let mut rules_paths = Vec::new();
    collect_rules_md_files(&rules_dir, &mut rules_paths);
    rules_paths.sort();
    let mut rules_contents = Vec::new();
    for rp in &rules_paths {
        if let Ok(c) = std::fs::read_to_string(rp) {
            rules_contents.push(c);
        }
    }
    let rules_contents_lower: Vec<String> =
        rules_contents.iter().map(|c| c.to_lowercase()).collect();

    // ── 4. rule expiry pre-checks (memory/rules.md, rules/**/*.md) ───────────
    let mut expiry_files = Vec::new();
    if mem_rules.is_file() {
        expiry_files.push(mem_rules);
    }
    expiry_files.extend(rules_paths.clone());
    expiry_files.sort();

    for f in expiry_files {
        if let Ok(content) = std::fs::read_to_string(&f) {
            let rel = f.strip_prefix(root).unwrap_or(&f);
            for (idx, line) in content.lines().enumerate() {
                match check_line_expiry(line, today) {
                    Some(ExpiryStatus::ValidExpired) => {
                        flags.push(format!(
                            "- expired rule: {}:{}: {}",
                            rel.display(),
                            idx + 1,
                            line.trim_end_matches(['\r', '\n'])
                        ));
                    }
                    Some(ExpiryStatus::Malformed) => {
                        flags.push(format!(
                            "- malformed expiry: {}:{}: {}",
                            rel.display(),
                            idx + 1,
                            line.trim_end_matches(['\r', '\n'])
                        ));
                    }
                    None => {}
                }
            }
        }
    }

    // ── 5. unpromoted lessons pre-checks (runs/*.md) ─────────────────────────
    if let Ok(entries) = std::fs::read_dir(&runs_dir) {
        let mut r_files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|name| name.ends_with(".md") && !name.ends_with(".rules.md"))
                    .unwrap_or(false)
            })
            .collect();
        r_files.sort();

        for f in r_files {
            let mtime_secs = match std::fs::metadata(&f).and_then(|m| m.modified()) {
                Ok(t) => t
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                Err(_) => continue,
            };
            let age_secs = now_secs.saturating_sub(mtime_secs);
            if age_secs >= 7 * 86_400 {
                continue;
            }

            let content = match std::fs::read_to_string(&f) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let mut in_lessons = false;
            let mut has_unpromoted = false;
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("## ") {
                    if trimmed.starts_with("## Lessons") {
                        in_lessons = true;
                        continue;
                    } else if in_lessons {
                        break;
                    }
                } else if in_lessons && trimmed.starts_with("Verdict:") {
                    break;
                }
                if in_lessons {
                    let clean = trimmed.trim_start_matches('-').trim();
                    let lower = clean.to_lowercase();
                    if !lower.is_empty() && lower != "none" && lower != "none." {
                        has_unpromoted = true;
                        break;
                    }
                }
            }

            if has_unpromoted {
                let base = f.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let needle = format!("runs/{base}.md");
                let cited = rules_contents.iter().any(|c| c.contains(&needle));
                if !cited {
                    flags.push(format!(
                        "- runs/{base}.md: '## Lessons' unpromoted from the last 7 days — no rules/ file cites runs/{base}.md"
                    ));
                }
            }
        }
    }

    // ── 6. casebook recurring failure pre-checks (memory/YYYY-MM-DD.md) ──────
    let mem_dir = root.join("memory");
    let mut mem_files: Vec<(u64, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&mem_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    if let Some((y, m, d)) = parse_ymd(stem) {
                        let f_days = ymd_to_days(y, m, d);
                        mem_files.push((f_days, p));
                    }
                }
            }
        }
    }

    let today_days = ymd_to_days(today.0, today.1, today.2);
    let mut cause_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for (f_days, path) in mem_files {
        if f_days <= today_days && (today_days - f_days) < 14 {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if let Some(rest) = trimmed.strip_prefix("- [case] ") {
                        if let Some((_prefix, cause_raw)) = rest.split_once(':') {
                            let cause = cause_raw.trim().to_lowercase();
                            if !cause.is_empty() {
                                *cause_counts.entry(cause).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    for (cause, count) in cause_counts {
        if count >= 2 {
            let cited = rules_contents_lower.iter().any(|c| c.contains(&cause));
            if !cited {
                flags.push(format!(
                    "- recurring failure ({count}x/14d, no rule): {cause}"
                ));
            }
        }
    }

    // ── 7. lead inbox timeline pre-checks (from DB triggers) ───────────────────
    let db_path = runs_dir.join("quaqua.db");
    if db_path.is_file() {
        let mut lead_workers: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        if let Ok(owner_entries) = std::fs::read_dir(&runs_dir) {
            for oe in owner_entries.flatten() {
                let op = oe.path();
                if let Some(fname) = op.file_name().and_then(|n| n.to_str()) {
                    if let Some(worker) = fname.strip_suffix(".owner") {
                        if !worker.starts_with('.') && !worker.ends_with("-lead") {
                            if let Ok(owner_content) = std::fs::read_to_string(&op) {
                                let owner = owner_content.trim();
                                if !owner.is_empty() {
                                    lead_workers
                                        .entry(owner.to_string())
                                        .or_default()
                                        .push(worker.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        for workers in lead_workers.values_mut() {
            workers.sort();
        }

        if let Ok(conn) = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            let farm = crate::workspace::farm_of_runs_dir(&runs_dir);
            struct TriggerEntry {
                id: u64,
                ts_ms: u64,
                actor: String,
                kind: String,
                body: String,
            }

            let stmt = conn.prepare(
                "SELECT id, ts_ms, actor, kind, payload_json FROM triggers WHERE source = 'mail' AND (to_name = 'cos' OR thread_id = 'agent:cos') AND (farm = ?1 OR farm IS NULL) ORDER BY id ASC",
            );
            if let Ok(mut stmt) = stmt {
                let rows = stmt.query_map([&farm], |row| {
                    let id = row.get::<_, i64>(0)? as u64;
                    let ts_ms = row.get::<_, i64>(1)? as u64;
                    let actor: String = row.get(2)?;
                    let kind: String = row.get(3)?;
                    let payload: Option<String> = row.get(4)?;
                    let body = payload
                        .as_deref()
                        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
                        .and_then(|v| {
                            v.get("body")
                                .and_then(|b| b.as_str().map(|s| s.to_string()))
                        })
                        .unwrap_or_else(|| payload.unwrap_or_default());
                    Ok(TriggerEntry {
                        id,
                        ts_ms,
                        actor,
                        kind,
                        body,
                    })
                });

                if let Ok(entries) = rows {
                    let mut lead_entries: std::collections::BTreeMap<String, Vec<TriggerEntry>> =
                        std::collections::BTreeMap::new();
                    for entry in entries.flatten() {
                        lead_entries
                            .entry(entry.actor.clone())
                            .or_default()
                            .push(entry);
                    }

                    for (lead_name, triggers) in lead_entries {
                        let slug = lead_name.strip_suffix("-lead").unwrap_or(&lead_name);

                        for entry in &triggers {
                            let cited_paths = extract_cited_md_paths(&entry.body);

                            if entry.kind == "accept" || entry.kind == "handoff" {
                                for cited in &cited_paths {
                                    let target_path = root.join(cited);
                                    if !target_path.is_file() {
                                        flags.push(format!(
                                            "- trigger #{} ({}): {} cites '{cited}' that does not exist",
                                            entry.id, entry.actor, entry.kind
                                        ));
                                    }
                                }
                            }

                            if entry.kind == "accept" {
                                for cited in &cited_paths {
                                    if cited.starts_with("runs/") {
                                        let target_path = root.join(cited);
                                        if let Ok(c) = std::fs::read_to_string(&target_path) {
                                            for l in c.lines() {
                                                let trimmed = l.trim();
                                                let lower = trimmed.to_ascii_lowercase();
                                                if let Some(rest) = lower.strip_prefix("outcome:") {
                                                    let val = rest.trim();
                                                    if val.starts_with("blocked")
                                                        || val.starts_with("disputed")
                                                    {
                                                        let out_word = if val.starts_with("blocked")
                                                        {
                                                            "BLOCKED"
                                                        } else {
                                                            "DISPUTED"
                                                        };
                                                        flags.push(format!(
                                                            "- trigger #{} ({}): accept cites '{cited}' with Outcome: {out_word}",
                                                            entry.id, entry.actor
                                                        ));
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        if let Some(last_entry) = triggers.last() {
                            let last_ts = last_entry.ts_ms / 1000;
                            let age_secs = now_secs.saturating_sub(last_ts);
                            if age_secs >= 86_400 {
                                let workers_by_lead = lead_workers.get(&lead_name);
                                let workers_by_slug = lead_workers.get(slug);
                                let mut all_workers = Vec::new();
                                if let Some(w) = workers_by_lead {
                                    all_workers.extend(w.clone());
                                }
                                if let Some(w) = workers_by_slug {
                                    all_workers.extend(w.clone());
                                }
                                all_workers.sort();
                                all_workers.dedup();

                                if !all_workers.is_empty() {
                                    flags.push(format!(
                                        "- lead {lead_name}: last entry is older than 24h (lost momentum) while owning live worker '{}'",
                                        all_workers.join(", ")
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quaqua_home_wins_over_quaqua_root() {
        // QUAQUA_HOME=A QUAQUA_ROOT=B must audit A, like every other command
        // (the auditor used to read QUAQUA_ROOT first).
        assert_eq!(
            resolve_auditor_root_from(
                None,
                None,
                Some(Path::new("A")),
                Some(Path::new("B")),
                Some(Path::new("home")),
            ),
            PathBuf::from("A"),
        );
    }

    #[test]
    fn explicit_and_auditor_repo_win_over_the_workspace() {
        assert_eq!(
            resolve_auditor_root_from(
                Some(Path::new("cli")),
                Some(Path::new("env")),
                Some(Path::new("A")),
                None,
                None,
            ),
            PathBuf::from("cli"),
        );
        assert_eq!(
            resolve_auditor_root_from(
                None,
                Some(Path::new("env")),
                Some(Path::new("A")),
                None,
                None,
            ),
            PathBuf::from("env"),
        );
    }

    #[test]
    fn test_prompt_template_has_all_keywords() {
        assert!(PROMPT_TEMPLATE.contains("READ-ONLY"));
        assert!(PROMPT_TEMPLATE.contains("memory/"));
        assert!(PROMPT_TEMPLATE.contains("projects/"));
        assert!(PROMPT_TEMPLATE.contains("git log"));
        assert!(PROMPT_TEMPLATE.contains("each repo listed in projects/INDEX.md"));
        assert!(PROMPT_TEMPLATE.contains("## Unhandled"));
        assert!(PROMPT_TEMPLATE.contains("[audit]"));
        assert!(PROMPT_TEMPLATE.contains("## Lessons"));
        assert!(PROMPT_TEMPLATE.contains("runs/"));
    }

    #[test]
    fn test_prompt_template_lessons_window_7_days() {
        assert!(PROMPT_TEMPLATE.contains("runs/*.md from the last 7 days"));
        assert!(!PROMPT_TEMPLATE.contains("runs/*.md from the last 2 days"));
    }
    struct TestDir(PathBuf);
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    impl std::ops::Deref for TestDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }
    impl AsRef<Path> for TestDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    fn test_temp_dir(tag: &str) -> TestDir {
        static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("quaqua-auditor-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TestDir(dir)
    }

    #[test]
    fn test_rule_expiry_expired_not_expired_malformed() {
        let root = test_temp_dir("expiry");
        let mem_dir = root.join("memory");
        let rules_dir = root.join("rules").join("_all");
        std::fs::create_dir_all(&mem_dir).unwrap();
        std::fs::create_dir_all(&rules_dir).unwrap();

        let mem_rules = mem_dir.join("rules.md");
        std::fs::write(
            &mem_rules,
            "- Valid expired rule [expires: 2026-10-23]\n- Valid future rule [expires: 2026-10-25]\n- Valid today rule [expires: 2026-10-24]\n",
        )
        .unwrap();

        let sample_rules = rules_dir.join("sample.md");
        std::fs::write(
            &sample_rules,
            "- Malformed date rule [expires: 2026-99-99]\n- Malformed text date rule [expires: not-a-date]\n- Normal permanent rule without expiry\n",
        )
        .unwrap();

        let today = (2026, 10, 24);
        let now_secs = ymd_to_days(2026, 10, 24) * 86400;
        let flags = run_deterministic_prechecks_at(&root, today, now_secs);

        assert!(
            flags.iter().any(|f| f.contains(
                "expired rule: memory/rules.md:1: - Valid expired rule [expires: 2026-10-23]"
            )),
            "expired rule on line 1 must be flagged: {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("memory/rules.md:2")),
            "future rule on line 2 must NOT be flagged: {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("memory/rules.md:3")),
            "today rule on line 3 must NOT be flagged: {:?}",
            flags
        );

        assert!(
            flags.iter().any(|f| f.contains(
                "malformed expiry: rules/_all/sample.md:1: - Malformed date rule [expires: 2026-99-99]"
            )),
            "malformed date rule on line 1 must be flagged with 'malformed expiry:': {:?}",
            flags
        );
        assert!(
            flags.iter().any(|f| f.contains("malformed expiry: rules/_all/sample.md:2: - Malformed text date rule [expires: not-a-date]")),
            "malformed text date rule on line 2 must be flagged with 'malformed expiry:': {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("rules/_all/sample.md:3")),
            "permanent rule without tag must NOT be flagged: {:?}",
            flags
        );
    }

    #[test]
    fn test_unpromoted_lessons_window_6_vs_8_days() {
        let root = test_temp_dir("lessons");
        let runs_dir = root.join("runs");
        let rules_dir = root.join("rules").join("_all");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&rules_dir).unwrap();

        let now_secs = ymd_to_days(2026, 10, 24) * 86400 + 43200;
        let today = (2026, 10, 24);

        // 6 days ago (recent: within 7 days)
        let p_recent = runs_dir.join("worker-recent.md");
        std::fs::write(&p_recent, "# Report\n\n## Lessons\n- Lesson to promote\n").unwrap();
        let f_recent = std::fs::File::open(&p_recent).unwrap();
        let mut times = std::fs::FileTimes::new();
        times = times.set_modified(
            std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(now_secs - 6 * 86400),
        );
        f_recent.set_times(times).unwrap();

        // 8 days ago (old: older than 7 days)
        let p_old = runs_dir.join("worker-old.md");
        std::fs::write(&p_old, "# Report\n\n## Lessons\n- Old lesson to ignore\n").unwrap();
        let f_old = std::fs::File::open(&p_old).unwrap();
        let mut times = std::fs::FileTimes::new();
        times = times.set_modified(
            std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(now_secs - 8 * 86400),
        );
        f_old.set_times(times).unwrap();

        // 6 days ago, but cited in rules/_all/promoted.md
        let p_cited = runs_dir.join("worker-cited.md");
        std::fs::write(&p_cited, "# Report\n\n## Lessons\n- Cited lesson\n").unwrap();
        let f_cited = std::fs::File::open(&p_cited).unwrap();
        let mut times = std::fs::FileTimes::new();
        times = times.set_modified(
            std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(now_secs - 6 * 86400),
        );
        f_cited.set_times(times).unwrap();
        std::fs::write(
            rules_dir.join("promoted.md"),
            "- Promoted rule — 2026-10-18, runs/worker-cited.md\n",
        )
        .unwrap();

        // 6 days ago, but lessons is none
        let p_none = runs_dir.join("worker-none.md");
        std::fs::write(&p_none, "# Report\n\n## Lessons\nnone\n").unwrap();
        let f_none = std::fs::File::open(&p_none).unwrap();
        let mut times = std::fs::FileTimes::new();
        times = times.set_modified(
            std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(now_secs - 6 * 86400),
        );
        f_none.set_times(times).unwrap();

        let flags = run_deterministic_prechecks_at(&root, today, now_secs);

        let agg: Vec<&String> = flags.iter().filter(|f| f.contains("unpromoted")).collect();
        assert_eq!(
            agg.len(),
            1,
            "one aggregate line for the one recent unpromoted lesson: {:?}",
            flags
        );
        assert!(
            agg[0].contains("1 run"),
            "aggregate counts worker-recent.md (6 days ago): {:?}",
            agg
        );
        assert!(
            !flags.iter().any(|f| f.contains("worker-old.md")),
            "worker-old.md (8 days ago) must NOT be flagged: {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("worker-cited.md")),
            "worker-cited.md (cited in rules) must NOT be flagged: {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("worker-none.md")),
            "worker-none.md (lessons none) must NOT be flagged: {:?}",
            flags
        );
    }

    #[test]
    fn test_unpromoted_lessons_stops_at_verdict_and_case_insensitive_none() {
        let root = test_temp_dir("verdict");
        let runs_dir = root.join("runs");
        let rules_dir = root.join("rules").join("_all");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&rules_dir).unwrap();

        let now_secs = ymd_to_days(2026, 10, 24) * 86400 + 43200;
        let today = (2026, 10, 24);

        // Reviewer report: ## Lessons\nnone\n\nVerdict: clean (within 7 days) -> MUST NOT be flagged
        let p_rev_clean = runs_dir.join("rev-clean.md");
        std::fs::write(
            &p_rev_clean,
            "# Review\n\n## Lessons\nnone\n\nVerdict: clean\n",
        )
        .unwrap();
        let f_rev_clean = std::fs::File::open(&p_rev_clean).unwrap();
        let mut times = std::fs::FileTimes::new();
        times = times.set_modified(
            std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(now_secs - 3 * 86400),
        );
        f_rev_clean.set_times(times).unwrap();

        // Reviewer report: ## Lessons\nNone\n\nVerdict: 1 findings (within 7 days) -> MUST NOT be flagged
        let p_rev_cap = runs_dir.join("rev-cap.md");
        std::fs::write(
            &p_rev_cap,
            "# Review\n\n## Lessons\nNone\n\nVerdict: 1 findings\n",
        )
        .unwrap();
        let f_rev_cap = std::fs::File::open(&p_rev_cap).unwrap();
        let mut times = std::fs::FileTimes::new();
        times = times.set_modified(
            std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(now_secs - 3 * 86400),
        );
        f_rev_cap.set_times(times).unwrap();

        // Reviewer report: ## Lessons\n- real lesson\n\nVerdict: clean (within 7 days) -> MUST be flagged
        let p_rev_lesson = runs_dir.join("rev-lesson.md");
        std::fs::write(
            &p_rev_lesson,
            "# Review\n\n## Lessons\n- real lesson\n\nVerdict: clean\n",
        )
        .unwrap();
        let f_rev_lesson = std::fs::File::open(&p_rev_lesson).unwrap();
        let mut times = std::fs::FileTimes::new();
        times = times.set_modified(
            std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(now_secs - 3 * 86400),
        );
        f_rev_lesson.set_times(times).unwrap();

        let flags = run_deterministic_prechecks_at(&root, today, now_secs);

        assert!(
            !flags.iter().any(|f| f.contains("rev-clean.md")),
            "rev-clean.md must NOT be flagged: {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("rev-cap.md")),
            "rev-cap.md must NOT be flagged: {:?}",
            flags
        );
        let agg: Vec<&String> = flags.iter().filter(|f| f.contains("unpromoted")).collect();
        assert!(
            agg.len() == 1 && agg[0].contains("1 run"),
            "only rev-lesson.md (real lesson) is counted: {:?}",
            flags
        );
    }

    #[test]
    fn test_casebook_recurring_failures_1x_vs_2x_and_cited() {
        let root = test_temp_dir("casebook");
        let mem_dir = root.join("memory");
        let rules_dir = root.join("rules").join("_all");
        std::fs::create_dir_all(&mem_dir).unwrap();
        std::fs::create_dir_all(&rules_dir).unwrap();

        std::fs::write(
            mem_dir.join("2026-10-20.md"),
            "- [case] worker-a exit 2: oom on cargo build\n- [case] worker-b reject: unhandled syntax error\n- [case] worker-c kill: runaway loop\n",
        )
        .unwrap();

        std::fs::write(
            mem_dir.join("2026-10-22.md"),
            "- [case] worker-d exit 2: oom on cargo build\n- [case] worker-e kill: runaway loop\n",
        )
        .unwrap();

        // Older than 14 days (today is 2026-10-24, 2026-10-01 is 23 days ago)
        std::fs::write(
            mem_dir.join("2026-10-01.md"),
            "- [case] worker-old exit 1: ancient failure\n",
        )
        .unwrap();

        // A rule citing "runaway loop"
        std::fs::write(
            rules_dir.join("loop-prevention.md"),
            "- Bound loop iterations to prevent runaway loop — 2026-10-15, runs/worker-c.md\n",
        )
        .unwrap();

        let today = (2026, 10, 24);
        let now_secs = ymd_to_days(2026, 10, 24) * 86400;
        let flags = run_deterministic_prechecks_at(&root, today, now_secs);

        assert!(
            flags
                .iter()
                .any(|f| f.contains("recurring failure (2x/14d, no rule): oom on cargo build")),
            "oom on cargo build (2x, no rule) must be flagged: {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("unhandled syntax error")),
            "unhandled syntax error (1x) must NOT be flagged: {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("runaway loop")),
            "runaway loop (cited in rules) must NOT be flagged: {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("ancient failure")),
            "ancient failure (>14d) must NOT be flagged: {:?}",
            flags
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_collect_rules_md_files_avoids_symlink_loop() {
        let root = test_temp_dir("symlink_loop");
        let rules_dir = root.join("rules");
        let sub = rules_dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(rules_dir.join("rule.md"), "# Rule\n").unwrap();
        let loop_link = sub.join("loop");
        let _ = std::os::unix::fs::symlink(&rules_dir, &loop_link);

        let mut out = Vec::new();
        collect_rules_md_files(&rules_dir, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], rules_dir.join("rule.md"));
    }

    #[test]
    fn lead_inbox_scan_ignores_other_farm_and_other_recipient() {
        let root = test_temp_dir("lead-inbox-scope");
        let runs_dir = root.join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(root.join("raw")).unwrap();
        let farm = crate::workspace::farm_of_runs_dir(&runs_dir);
        let now_secs = 1_800_000_000u64;
        let today = days_to_ymd(now_secs / 86400);

        let conn = rusqlite::Connection::open(runs_dir.join("quaqua.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, owner TEXT NOT NULL, state TEXT NOT NULL, last_trigger_ms INTEGER NOT NULL);
             CREATE TABLE triggers (id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, source TEXT NOT NULL, kind TEXT NOT NULL, actor TEXT NOT NULL, to_name TEXT, thread_id TEXT NOT NULL, summary TEXT NOT NULL, ref TEXT, payload_json TEXT, dedupe_key TEXT UNIQUE, farm TEXT NOT NULL);",
        )
        .unwrap();
        let ins = |actor: &str, to: &str, thread: &str, farm: &str| {
            conn.execute(
                "INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, payload_json, farm) \
                 VALUES (?1, 'mail', 'accept', ?2, ?3, ?4, 's', ?5, ?6)",
                rusqlite::params![
                    (now_secs - 3600) as i64 * 1000,
                    actor,
                    to,
                    thread,
                    serde_json::json!({ "body": "Accepted runs/ghost.md" }).to_string(),
                    farm,
                ],
            )
            .unwrap();
        };
        ins("mine-lead", "cos", "t", &farm);
        ins("other-farm-lead", "cos", "t", "some-other-farm");
        ins("other-rcpt-lead", "someone-else", "t", &farm);

        let flags = run_deterministic_prechecks_at(&root, today, now_secs);
        assert!(
            flags.iter().any(|f| f.contains("mine-lead")),
            "own-farm mail to cos must be scanned: {flags:?}"
        );
        assert!(
            !flags.iter().any(|f| f.contains("other-farm-lead")),
            "other-farm mail must not be scanned: {flags:?}"
        );
        assert!(
            !flags.iter().any(|f| f.contains("other-rcpt-lead")),
            "mail to another recipient must not be scanned: {flags:?}"
        );
    }

    #[test]
    fn lead_inbox_precheck() {
        // Assert item 6 keywords in PROMPT_TEMPLATE
        assert!(
            PROMPT_TEMPLATE.contains("Lead inbox timeline"),
            "PROMPT_TEMPLATE must cite Lead inbox timeline"
        );
        assert!(
            PROMPT_TEMPLATE.contains("authority-gradient"),
            "PROMPT_TEMPLATE must cite authority-gradient"
        );
        assert!(
            PROMPT_TEMPLATE.contains("self-acceptance"),
            "PROMPT_TEMPLATE must cite self-acceptance"
        );
        assert!(
            PROMPT_TEMPLATE.contains("lost-momentum"),
            "PROMPT_TEMPLATE must cite lost-momentum"
        );
        assert!(
            PROMPT_TEMPLATE.contains("file:line"),
            "PROMPT_TEMPLATE must cite file:line"
        );

        let root = test_temp_dir("lead-inbox-precheck");
        let runs_dir = root.join("runs");
        let raw_dir = root.join("raw");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&raw_dir).unwrap();

        let now_secs = 1_800_000_000u64;
        let today = days_to_ymd(now_secs / 86400);

        let conn = rusqlite::Connection::open(runs_dir.join("quaqua.db")).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                owner TEXT NOT NULL,
                state TEXT NOT NULL,
                last_trigger_ms INTEGER NOT NULL
            );
            CREATE TABLE triggers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts_ms INTEGER NOT NULL,
                source TEXT NOT NULL,
                kind TEXT NOT NULL,
                actor TEXT NOT NULL,
                to_name TEXT,
                thread_id TEXT NOT NULL,
                summary TEXT NOT NULL,
                ref TEXT,
                payload_json TEXT,
                dedupe_key TEXT UNIQUE,
                farm TEXT NOT NULL
            );
            "#,
        )
        .unwrap();

        // 1. Clean lead: valid accept of existing COMPLETE run, recent timestamp
        let clean_worker = runs_dir.join("clean-worker.md");
        std::fs::write(&clean_worker, "Outcome: COMPLETE\n## Lessons\nnone\n").unwrap();
        conn.execute(
            "INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, payload_json, farm) \
             VALUES (?1, 'mail', 'accept', 'clean-lead', 'cos', 't', 'clean', ?2, 'test')",
            rusqlite::params![
                (now_secs - 3600) as i64 * 1000,
                serde_json::json!({ "body": "Accepted clean work in runs/clean-worker.md" }).to_string(),
            ],
        )
        .unwrap();

        // 2. Fixture (a): missing runs/<x>.md and missing raw/<x>.md
        conn.execute(
            "INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, payload_json, farm) \
             VALUES (?1, 'mail', 'accept', 'missing-lead', 'cos', 't', 'missing-accept', ?2, 'test')",
            rusqlite::params![
                (now_secs - 7200) as i64 * 1000,
                serde_json::json!({ "body": "Accepted runs/nonexistent-worker.md with verified sha" }).to_string(),
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, payload_json, farm) \
             VALUES (?1, 'mail', 'handoff', 'missing-lead', 'cos', 't', 'missing-handoff', ?2, 'test')",
            rusqlite::params![
                (now_secs - 3600) as i64 * 1000,
                serde_json::json!({ "body": "Handoff notes in raw/missing-notes.md" }).to_string(),
            ],
        )
        .unwrap();

        // 3. Fixture (b): accept citing runs file with Outcome: BLOCKED or DISPUTED
        let blocked_worker = runs_dir.join("blocked-worker.md");
        std::fs::write(
            &blocked_worker,
            "Outcome: BLOCKED\n## Blockers\nstuck\n## Lessons\nnone\n",
        )
        .unwrap();
        let disputed_worker = runs_dir.join("disputed-worker.md");
        std::fs::write(
            &disputed_worker,
            "Outcome: DISPUTED\n## Blockers\nnone\n## Lessons\nnone\n",
        )
        .unwrap();

        conn.execute(
            "INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, payload_json, farm) \
             VALUES (?1, 'mail', 'accept', 'blocked-lead', 'cos', 't', 'blocked-accept', ?2, 'test')",
            rusqlite::params![
                (now_secs - 7200) as i64 * 1000,
                serde_json::json!({ "body": "Accepted runs/blocked-worker.md" }).to_string(),
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, payload_json, farm) \
             VALUES (?1, 'mail', 'accept', 'blocked-lead', 'cos', 't', 'disputed-accept', ?2, 'test')",
            rusqlite::params![
                (now_secs - 3600) as i64 * 1000,
                serde_json::json!({ "body": "Accepted runs/disputed-worker.md" }).to_string(),
            ],
        )
        .unwrap();

        // 4. Fixture (c): last entry older than 24h while owning live worker
        std::fs::write(runs_dir.join("sude-active.owner"), "momentum-lead\n").unwrap();
        conn.execute(
            "INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, payload_json, farm) \
             VALUES (?1, 'mail', 'handoff', 'momentum-lead', 'cos', 't', 'momentum-handoff', ?2, 'test')",
            rusqlite::params![
                (now_secs - 100_000) as i64 * 1000,
                serde_json::json!({ "body": "Old handoff from yesterday" }).to_string(),
            ],
        )
        .unwrap();

        conn.execute(
            "UPDATE triggers SET farm = ?1",
            [crate::workspace::farm_of_runs_dir(&runs_dir)],
        )
        .unwrap();

        let flags = run_deterministic_prechecks_at(&root, today, now_secs);

        // Clean lead has no findings
        assert!(
            !flags.iter().any(|f| f.contains("clean-lead")),
            "clean lead must not be flagged: {:?}",
            flags
        );

        // (a) Missing files flagged
        assert!(
            flags.iter().any(|f| f.contains("missing-lead")
                && f.contains("runs/nonexistent-worker.md")
                && f.contains("does not exist")),
            "missing runs file must be flagged for (a): {:?}",
            flags
        );
        assert!(
            flags.iter().any(|f| f.contains("missing-lead")
                && f.contains("raw/missing-notes.md")
                && f.contains("does not exist")),
            "missing raw file must be flagged for (a): {:?}",
            flags
        );

        // (b) Accept over BLOCKED or DISPUTED flagged
        assert!(
            flags.iter().any(|f| f.contains("blocked-lead")
                && f.contains("runs/blocked-worker.md")
                && f.contains("Outcome: BLOCKED")),
            "accept over BLOCKED must be flagged for (b): {:?}",
            flags
        );
        assert!(
            flags.iter().any(|f| f.contains("blocked-lead")
                && f.contains("runs/disputed-worker.md")
                && f.contains("Outcome: DISPUTED")),
            "accept over DISPUTED must be flagged for (b): {:?}",
            flags
        );

        // (c) Older than 24h with live worker flagged
        assert!(
            flags.iter().any(|f| f.contains("momentum-lead")
                && f.contains("older than 24h")
                && f.contains("sude-active")),
            "lost momentum (>24h with live worker) must be flagged for (c): {:?}",
            flags
        );
    }

    fn backdate(path: &Path, secs_ago: u64, now_secs: u64) {
        let f = std::fs::File::open(path).unwrap();
        let t = std::fs::FileTimes::new().set_modified(
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(now_secs - secs_ago),
        );
        f.set_times(t).unwrap();
    }

    fn git_init_commit(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "git {args:?} failed");
            String::from_utf8(o.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "c1"]);
        git(&["rev-parse", "--short", "HEAD"])
    }

    #[test]
    fn test_many_unpromoted_lessons_collapse_to_one_count_line() {
        let root = test_temp_dir("lessons-agg");
        let runs_dir = root.join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        let now_secs = ymd_to_days(2026, 11, 13) * 86400 + 43200;
        for i in 0..5 {
            std::fs::write(
                runs_dir.join(format!("w{i}.md")),
                "# R\n\n## Lessons\n- something learned\n",
            )
            .unwrap();
            backdate(&runs_dir.join(format!("w{i}.md")), 86400, now_secs);
        }
        let flags = run_deterministic_prechecks_at(&root, (2026, 11, 13), now_secs);
        let agg: Vec<&String> = flags.iter().filter(|f| f.contains("unpromoted")).collect();
        assert_eq!(agg.len(), 1, "5 lessons -> one line, not five: {:?}", flags);
        assert!(agg[0].contains("5 runs"), "count is stated: {:?}", agg);
        assert!(
            !flags.iter().any(|f| f.contains("w3.md")),
            "no per-file lesson line: {:?}",
            flags
        );
    }

    #[test]
    fn test_old_pipeline_runs_counted_not_listed_fresh_ones_flagged() {
        let root = test_temp_dir("pipe-old");
        let runs_dir = root.join("runs");
        let mem_dir = root.join("memory");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&mem_dir).unwrap();
        let repo = root.join("proj");
        let _ = git_init_commit(&repo);
        let now_secs = ymd_to_days(2026, 11, 13) * 86400 + 43200;
        let put = |name: &str, started: &str, status: &str| {
            std::fs::write(
                runs_dir.join(format!("pipeline-{name}.md")),
                format!(
                    "# pipeline {name}\n- task: {name} \u{b7} repo: {}\n- started: {started}T10:00:00+07:00\n- {status}\n",
                    repo.display()
                ),
            )
            .unwrap();
        };
        // old (started 2026-10-19): bad sha in a real repo, unhandled blocked, handled blocked
        put(
            "oldbad",
            "2026-10-19",
            "status: clean \u{b7} commit deadbee \u{b7} elapsed 1m",
        );
        put("oldblk", "2026-10-19", "status: blocked");
        put("oldok", "2026-10-19", "status: blocked");
        std::fs::write(mem_dir.join("2026-10-20.md"), "- pipeline-oldok handled\n").unwrap();
        // fresh (started 2026-11-12): same defects
        put(
            "freshbad",
            "2026-11-12",
            "status: clean \u{b7} commit deadbee \u{b7} elapsed 1m",
        );
        put("freshblk", "2026-11-12", "status: blocked");

        let flags = run_deterministic_prechecks_at(&root, (2026, 11, 13), now_secs);
        assert!(
            flags
                .iter()
                .any(|f| f.contains("pipeline-freshbad.md") && f.contains("does not resolve")),
            "fresh bad sha in a real repo is still flagged: {:?}",
            flags
        );
        assert!(
            flags
                .iter()
                .any(|f| f.contains("pipeline-freshblk.md") && f.contains("no memory")),
            "fresh unhandled blocked run is still listed: {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("pipeline-oldbad.md")),
            "old clean run is not checked per sha: {:?}",
            flags
        );
        assert!(
            !flags.iter().any(|f| f.contains("pipeline-oldok")),
            "old handled blocked run is silent: {:?}",
            flags
        );
        let agg: Vec<&String> = flags.iter().filter(|f| f.contains("older than")).collect();
        assert_eq!(agg.len(), 1, "one aggregate line for old runs: {:?}", flags);
        assert!(
            agg[0].contains("pipeline-oldblk") && agg[0].starts_with("- runs/pipeline-"),
            "old unhandled blocked run is named in the aggregate: {:?}",
            agg
        );
    }

    #[test]
    fn test_recorded_repo_not_a_git_repo_is_one_line_per_repo() {
        let root = test_temp_dir("pipe-nogit");
        let runs_dir = root.join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        let dead = root.join("moved-away");
        std::fs::create_dir_all(&dead).unwrap();
        let live = root.join("live");
        let _ = git_init_commit(&live);
        let now_secs = ymd_to_days(2026, 11, 13) * 86400 + 43200;
        for (i, sha) in ["aaa1111", "bbb2222", "ccc3333"].iter().enumerate() {
            std::fs::write(
                runs_dir.join(format!("pipeline-d{i}.md")),
                format!(
                    "- task: d{i} \u{b7} repo: {}\n- started: 2026-11-12T10:00:00+07:00\n- status: clean \u{b7} commit {sha}\n",
                    dead.display()
                ),
            )
            .unwrap();
        }
        std::fs::write(
            runs_dir.join("pipeline-live1.md"),
            format!(
                "- task: live1 \u{b7} repo: {}\n- started: 2026-11-12T10:00:00+07:00\n- status: clean \u{b7} commit badbad0\n",
                live.display()
            ),
        )
        .unwrap();
        let flags = run_deterministic_prechecks_at(&root, (2026, 11, 13), now_secs);
        let dead_lines: Vec<&String> = flags
            .iter()
            .filter(|f| f.contains(&*dead.display().to_string()))
            .collect();
        assert_eq!(
            dead_lines.len(),
            1,
            "3 runs, one repo -> one line: {:?}",
            flags
        );
        assert!(
            dead_lines[0].contains("not a git repo") && dead_lines[0].contains("3 runs"),
            "{:?}",
            dead_lines
        );
        assert!(
            flags
                .iter()
                .any(|f| f.contains("pipeline-live1.md") && f.contains("does not resolve")),
            "bad sha in a real git repo still flagged per run: {:?}",
            flags
        );
    }
}
