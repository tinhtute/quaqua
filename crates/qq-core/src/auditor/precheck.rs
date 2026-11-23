//! Deterministic pre-checks: the cheap greps that run before the model does.
//! Read-only over the workspace; every flag is one `- ...` line.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::clock::{days_from_civil, now_sec, ymd_from_days};

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    ymd_from_days(days)
}

fn ymd_to_days(year: u64, month: u64, day: u64) -> u64 {
    days_from_civil(year as i64, month, day)
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

/// True when `dir` is inside a git work tree (git discovery, so a repo subdirectory counts).
fn git_is_repo(dir: &Path) -> bool {
    let res = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--git-dir"])
        .output();
    matches!(res, Ok(output) if output.status.success())
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

const PIPELINE_CUTOFF_DAYS: u64 = 14;

pub fn run_deterministic_prechecks(root: &Path) -> Vec<String> {
    let now_secs = now_sec();
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
    let days = days_from_civil(y as i64, m as u64, d as u64);
    if ymd_from_days(days) == (y as u64, m as u64, d as u64) {
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
    let today_days = ymd_to_days(today.0, today.1, today.2);
    // Runs that started more than PIPELINE_CUTOFF_DAYS ago are history: counted,
    // never checked one per line. Dated by the in-file `- started:` line, else mtime.
    let mut old_skipped = 0usize;
    let mut old_unhandled: Vec<String> = Vec::new();
    // recorded repo -> (first run, run count, why it cannot verify commits): one line per repo.
    let mut dead_repos: std::collections::BTreeMap<String, (String, usize, &str)> =
        std::collections::BTreeMap::new();
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

            let started_days = content
                .lines()
                .find_map(|l| l.strip_prefix("- started: "))
                .and_then(|v| v.get(..10))
                .and_then(parse_ymd)
                .map(|(y, m, d)| ymd_to_days(y, m, d));
            let mtime_days = std::fs::metadata(&f)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() / 86_400);
            // Old = every date the file carries is old: a late resume that appends to an
            // old run touches its mtime, and that run stays listed per file.
            let is_old = |d: Option<u64>| {
                d.is_none_or(|d| today_days.saturating_sub(d) > PIPELINE_CUTOFF_DAYS)
            };
            if (started_days.is_some() || mtime_days.is_some())
                && is_old(started_days)
                && is_old(mtime_days)
            {
                old_skipped += 1;
                if matches!(st, "disputed" | "blocked") && !mem_mentions(root, &[base]) {
                    old_unhandled.push(base.to_string());
                }
                continue;
            }

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

                    // `.git` must be in the repo itself: git discovery would walk up to a parent repo.
                    if !prepo.is_dir() || !git_is_repo(&prepo) {
                        let why = if prepo.is_dir() {
                            "not a git repo"
                        } else {
                            "missing"
                        };
                        let e = dead_repos
                            .entry(prepo.display().to_string())
                            .or_insert_with(|| (base.to_string(), 0, why));
                        e.1 += 1;
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
    for (repo, (first, n, why)) in dead_repos {
        if n == 1 {
            flags.push(format!(
                "- runs/{first}.md: status clean but recorded repo '{repo}' is {why}"
            ));
        } else {
            flags.push(format!(
                "- runs/{first}.md ({n} runs, first listed): status clean but recorded repo '{repo}' is {why}; their commits cannot be checked"
            ));
        }
    }
    if !old_unhandled.is_empty() {
        let who = if old_unhandled.len() <= 5 {
            format!(": {}", old_unhandled.join(", "))
        } else {
            String::new()
        };
        flags.push(format!(
            "- runs/pipeline-*.md: {old_skipped} pipeline runs older than {PIPELINE_CUTOFF_DAYS} days are not checked; {} of them blocked/disputed with no memory line naming them{who}",
            old_unhandled.len()
        ));
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
            if !crate::task::report_contract::disputes(&content) {
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
    let mut unpromoted = 0usize;
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
                    unpromoted += 1;
                }
            }
        }
        if unpromoted > 0 {
            flags.push(format!(
                "- runs/*.md: {} unpromoted '## Lessons' from the last 7 days (CoS promotes at most one per cleanup; not listed per file)",
                if unpromoted == 1 { "1 run has".to_string() } else { format!("{unpromoted} runs have") }
            ));
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

    // ── 7. lead inbox timeline pre-checks (mail to cos in qq.db) ──────────────
    mail_checks(root, &runs_dir, now_secs, &mut flags);

    flags
}

struct MailEntry {
    id: i64,
    ts_ms: u64,
    sender: String,
    kind: String,
    body: String,
}

/// Opens `<runs>/qq.db` read-only. With no `-wal` beside it nothing writes it,
/// so it opens immutable and sqlite creates no `-wal`/`-shm` file.
fn open_ro(db: &Path) -> Option<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    if Path::new(&wal).exists() {
        return rusqlite::Connection::open_with_flags(db, flags).ok();
    }
    let uri = format!(
        "file:{}?immutable=1",
        db.to_string_lossy()
            .replace('%', "%25")
            .replace('?', "%3f")
            .replace('#', "%23")
    );
    rusqlite::Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI).ok()
}

/// Section 7: Lead-to-CoS mail timelines. A missing database, table or
/// unseeded schema is silent (nothing to check).
fn mail_checks(root: &Path, runs_dir: &Path, now_secs: u64, flags: &mut Vec<String>) {
    let db = runs_dir.join("qq.db");
    if !db.is_file() {
        return;
    }
    let Some(conn) = open_ro(&db) else { return };
    let Ok(mut st) = conn.prepare(
        "SELECT id, created_ms, sender, kind, body FROM mail WHERE recipient = 'cos' ORDER BY id ASC",
    ) else {
        return;
    };
    let Ok(rows) = st.query_map([], |r| {
        Ok(MailEntry {
            id: r.get(0)?,
            ts_ms: r.get::<_, i64>(1)?.max(0) as u64,
            sender: r.get(2)?,
            kind: r.get(3)?,
            body: r.get(4)?,
        })
    }) else {
        return;
    };
    let mut by_sender: std::collections::BTreeMap<String, Vec<MailEntry>> =
        std::collections::BTreeMap::new();
    for e in rows.flatten() {
        by_sender.entry(e.sender.clone()).or_default().push(e);
    }
    for (sender, entries) in by_sender {
        for entry in &entries {
            let cited_paths = extract_cited_md_paths(&entry.body);
            if entry.kind == "accept" || entry.kind == "handoff" {
                for cited in &cited_paths {
                    if !root.join(cited).is_file() {
                        flags.push(format!(
                            "- mail #{} ({}): {} cites '{cited}' that does not exist",
                            entry.id, entry.sender, entry.kind
                        ));
                    }
                }
            }
            if entry.kind == "accept" {
                for cited in cited_paths.iter().filter(|c| c.starts_with("runs/")) {
                    let Ok(c) = std::fs::read_to_string(root.join(cited)) else {
                        continue;
                    };
                    for l in c.lines() {
                        let lower = l.trim().to_ascii_lowercase();
                        let Some(rest) = lower.strip_prefix("outcome:") else {
                            continue;
                        };
                        let val = rest.trim();
                        if val.starts_with("blocked") || val.starts_with("disputed") {
                            let w = if val.starts_with("blocked") {
                                "BLOCKED"
                            } else {
                                "DISPUTED"
                            };
                            flags.push(format!(
                                "- mail #{} ({}): accept cites '{cited}' with Outcome: {w}",
                                entry.id, entry.sender
                            ));
                            break;
                        }
                    }
                }
            }
        }
        let Some(last) = entries.last() else { continue };
        if now_secs.saturating_sub(last.ts_ms / 1000) < 86_400 {
            continue;
        }
        let live: Vec<String> = conn
            .prepare(
                "SELECT w.name FROM agent w JOIN agent l ON w.spawner_id = l.id \
                 WHERE l.name = ?1 AND l.role = 'lead' AND w.ended_ms IS NULL ORDER BY w.name",
            )
            .and_then(|mut q| {
                q.query_map([&sender], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_default();
        if !live.is_empty() {
            flags.push(format!(
                "- lead {sender}: last entry is older than 24h (lost momentum) while owning live worker '{}'",
                live.join(", ")
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags_for(tag: &str, report: &str) -> Vec<String> {
        let root = std::env::temp_dir().join(format!("qq-precheck-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("runs")).unwrap();
        std::fs::create_dir_all(root.join("memory")).unwrap();
        std::fs::write(root.join("runs/w1.md"), report).unwrap();
        let flags = run_deterministic_prechecks_at(&root, (2026, 11, 23), 1_790_000_000);
        let _ = std::fs::remove_dir_all(&root);
        flags
    }

    #[test]
    fn precheck_ignores_a_challenge_that_disputes_nothing() {
        for (i, r) in [
            "## Findings\n(none)\n\n## Challenge\nNone. The brief's premises hold.\n\nVerdict: clean",
            "## Challenge\nNone. Premises hold: commit exists on quaqua/sude-e2e1-b and is the HEAD of the review worktree.\n\nVerdict: clean",
        ]
        .iter()
        .enumerate()
        {
            let f = flags_for(&format!("none{i}"), r);
            assert!(!f.iter().any(|l| l.contains("'## Challenge'")), "{f:?}");
        }
    }

    #[test]
    fn precheck_flags_a_real_challenge() {
        let f = flags_for(
            "real",
            "## Challenge\nNone of the premises hold: no such commit.\n",
        );
        assert!(
            f.iter()
                .any(|l| l.contains("runs/w1.md: '## Challenge' unresolved")),
            "{f:?}"
        );
    }
}
