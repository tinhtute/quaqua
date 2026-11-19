//! The cleanup journal: the memory lines and the `raw/` copy the CoS used to
//! write by hand after each worker (`- [slug] <worker> <outcome>`, `- [case]
//! <worker> exit N: <cause>`). Every write is idempotent: a rerun finds its own
//! trace and writes nothing. The tool line states only what the tool saw (the
//! report's Outcome line, the branch tip, the raw path), never a CoS verdict.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What a cleanup journaled, for the verb's reply.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Journaled {
    /// The memory line appended now, if any.
    pub line: Option<String>,
    /// The `raw/` file the line cites (new or reused), if the report existed.
    pub raw: Option<String>,
    /// Why nothing was journaled (no workspace layout).
    pub skipped: Option<String>,
}

/// The workspace root for a `runs/` dir: its parent, when it has a `memory/`.
fn workspace_of(runs: &Path) -> Option<PathBuf> {
    let root = runs.parent()?;
    root.join("memory").is_dir().then(|| root.to_path_buf())
}

/// Local `YYYY-MM-DD` (as `date +%F`), UTC when `date` cannot run.
pub fn today() -> String {
    if let Ok(o) = Command::new("date").arg("+%F").output() {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if o.status.success() && s.len() == 10 {
            return s;
        }
    }
    let z = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        / 86_400) as i64
        + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", yoe + era * 400 + i64::from(m <= 2))
}

/// The project slug: the one `projects/INDEX.md` row whose repo is `repo`;
/// with several (one repo can host many projects) the single `active` one;
/// with none, or no single active row, the repo dir name.
fn slug_of(root: &Path, repo: &Path) -> String {
    let fallback = repo
        .file_name()
        .map_or_else(|| "repo".to_string(), |f| f.to_string_lossy().into_owned());
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let want = canon(repo);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let Ok(text) = std::fs::read_to_string(root.join("projects/INDEX.md")) else {
        return fallback;
    };
    let hits: Vec<(&str, bool)> = text
        .lines()
        .filter_map(|l| {
            let mut c = l.trim().trim_matches('|').split('|').map(str::trim);
            let (slug, path) = (c.next()?, c.next()?);
            let active = c.nth(1).is_some_and(|st| st.starts_with("active"));
            let path = match (path.strip_prefix("~/"), &home) {
                (Some(rest), Some(h)) => h.join(rest),
                _ => PathBuf::from(path),
            };
            (!slug.starts_with('_') && canon(&path) == want).then_some((slug, active))
        })
        .collect();
    let active: Vec<_> = hits.iter().filter(|h| h.1).collect();
    match (hits.as_slice(), active.as_slice()) {
        ([one], _) => one.0.to_string(),
        (_, [one]) => one.0.to_string(),
        _ => fallback,
    }
}

/// `sude-mainguard` -> `mainguard`: the task part of a worker name.
fn task_of(name: &str) -> &str {
    name.split_once('-').map_or(name, |(_, t)| t)
}

/// The Outcome value of a report (first `Outcome:` line at column 1).
fn outcome_of(report: &str) -> Option<String> {
    let v = report.lines().find_map(|l| l.strip_prefix("Outcome:"))?;
    let v: String = v.trim().chars().take(80).collect();
    (!v.is_empty()).then_some(v)
}

/// Appends `line` to `memory/<today>.md` unless that exact line is already in
/// any dated memory file. Returns whether it was written.
fn append_once(root: &Path, today: &str, line: &str, all_days: bool) -> bool {
    let dir = root.join("memory");
    let seen = |p: &Path| {
        std::fs::read_to_string(p).is_ok_and(|t| t.lines().any(|l| l.trim_end() == line))
    };
    let file = dir.join(format!("{today}.md"));
    let dup = if all_days {
        std::fs::read_dir(&dir).is_ok_and(|rd| {
            rd.flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == "md") && seen(&e.path()))
        })
    } else {
        seen(&file)
    };
    if dup {
        return false;
    }
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    std::fs::write(&file, text).is_ok()
}

/// Copies `report` to `raw/<today>-<slug>-<task>.md`, never overwriting: an
/// earlier copy with the same bytes is reused, a different one gets `-2`, `-3`.
fn raw_copy(root: &Path, today: &str, slug: &str, task: &str, report: &str) -> Option<String> {
    let raw = root.join("raw");
    std::fs::create_dir_all(&raw).ok()?;
    let tail = format!("-{slug}-{task}");
    for e in std::fs::read_dir(&raw).ok()?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let stem = n.strip_suffix(".md").unwrap_or("");
        let base = stem
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .trim_end_matches('-');
        let ours = [stem, base]
            .iter()
            .any(|s| s.len() >= tail.len() + 10 && s.ends_with(&tail));
        if ours && std::fs::read_to_string(e.path()).is_ok_and(|t| t == report) {
            return Some(n);
        }
    }
    for i in 1.. {
        let n = if i == 1 {
            format!("{today}{tail}.md")
        } else {
            format!("{today}{tail}-{i}.md")
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(raw.join(&n))
        {
            Ok(mut f) => {
                use std::io::Write;
                return f.write_all(report.as_bytes()).ok().map(|()| n);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Journals one cleaned worker: copies `runs/<name>.md` to `raw/` and appends
/// `- [<slug>] <name> cleanup: <Outcome> <sha> · raw/<file>` (a missing report
/// writes no copy and says `no report`). `sha` is the branch tip, `-` unknown.
pub fn cleanup(runs: &Path, repo: &Path, name: &str, sha: &str, today: &str) -> Journaled {
    let Some(root) = workspace_of(runs) else {
        return Journaled {
            skipped: Some(format!("no workspace memory/ beside {}", runs.display())),
            ..Journaled::default()
        };
    };
    let slug = slug_of(&root, repo);
    let report = std::fs::read_to_string(runs.join(format!("{name}.md"))).ok();
    let raw = report
        .as_deref()
        .and_then(|r| raw_copy(&root, today, &slug, task_of(name), r));
    let what = match (&report, &raw) {
        (Some(r), Some(f)) => format!(
            "{} {sha} · raw/{f}",
            outcome_of(r).unwrap_or_else(|| "no Outcome line".into())
        ),
        (Some(_), None) => format!("report not copied (raw/ write failed) {sha}"),
        (None, _) => format!("no report {sha}"),
    };
    let line = format!("- [{slug}] {name} cleanup: {what}");
    let written = append_once(&root, today, &line, true);
    Journaled {
        line: written.then_some(line),
        raw,
        skipped: None,
    }
}

/// The cause a `worker prompt` watchdog exit stands for; `None` for the exits
/// that are not failures worth a case line.
fn cause(code: i32) -> Option<&'static str> {
    Some(match code {
        3 => "stalled (no change or idle past the limit)",
        4 => "budget exceeded",
        5 => "blocked on a prompt",
        7 => "provider error",
        8 => "core died",
        _ => return None,
    })
}

/// Appends `- [case] <name> exit N: <cause>` for a watchdog exit 3/4/5/7/8 to
/// today's memory; the same line twice in one day is written once.
pub fn watchdog_case(runs: &Path, name: &str, code: i32) -> Option<String> {
    let cause = cause(code)?;
    let root = workspace_of(runs)?;
    let line = format!("- [case] {name} exit {code}: {cause}");
    append_once(&root, &today(), &line, false).then_some(line)
}
