//! `qq transcripts tail`: the last N things the Principal said in one Claude
//! Code session, read straight from its JSONL (no index). The pre-compaction
//! snapshot uses it. The corpus is read-only.

mod parse;

use std::path::{Path, PathBuf};

use crate::cli::{CliError, Command, Env, Install, Registry};
use crate::daemon::Daemon;

/// Messages `tail` prints when `--last` is not given.
pub const DEFAULT_LAST: usize = 10;

/// The entry `cli::MODULES` lists.
pub const INSTALL: Install = install;

fn install(registry: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "transcripts",
        help: "transcripts tail --session <id|path> [--last N] [--projects DIR]",
        run: transcripts_cmd,
    })
}

fn usage() -> CliError {
    CliError::new(
        2,
        "usage: qq transcripts tail --session <id|path> [--last N] [--projects DIR]",
    )
}

fn transcripts_cmd(_env: &Env, args: &[String]) -> Result<String, CliError> {
    let Some((sub, rest)) = args.split_first() else {
        return Err(usage());
    };
    if sub != "tail" {
        return Err(usage());
    }
    let (mut session, mut last, mut projects) = (None, DEFAULT_LAST, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--session" => session = Some(it.next().ok_or_else(usage)?.clone()),
            "--last" => last = it.next().and_then(|v| v.parse().ok()).ok_or_else(usage)?,
            "--projects" => projects = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            _ => return Err(usage()),
        }
    }
    let session = session.ok_or_else(usage)?;
    tail(&session, last, projects.as_deref())
}

/// The corpus root: `~/.claude/projects` unless overridden.
pub fn projects_root(override_dir: Option<&Path>) -> PathBuf {
    if let Some(p) = override_dir {
        return p.to_path_buf();
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".claude").join("projects"),
        None => PathBuf::from(".claude").join("projects"),
    }
}

/// The last `last` main-conversation user messages of `session`, one per line.
pub fn tail(session: &str, last: usize, projects: Option<&Path>) -> Result<String, CliError> {
    let root = projects_root(projects);
    let path = resolve_session(session, &root)
        .ok_or_else(|| CliError::new(1, format!("transcripts tail: unknown session {session}")))?;
    let scan = parse::read_entries(&path).map_err(|e| {
        CliError::new(
            1,
            format!("transcripts tail: reading {}: {e}", path.display()),
        )
    })?;
    let prompts: Vec<&parse::Entry> = scan
        .entries
        .iter()
        .filter(|e| e.role == "user" && !e.sidechain)
        .collect();
    let start = prompts.len().saturating_sub(last);
    let lines: Vec<String> = prompts[start..].iter().map(|e| collapse(&e.text)).collect();
    Ok(lines.join("\n"))
}

/// `--session`: a path when it looks like one, else `<root>/*/<id>.jsonl`.
fn resolve_session(arg: &str, root: &Path) -> Option<PathBuf> {
    let as_path = PathBuf::from(arg);
    if arg.contains('/') || arg.ends_with(".jsonl") || as_path.is_file() {
        return as_path.is_file().then_some(as_path);
    }
    for dir in std::fs::read_dir(root).ok()?.flatten() {
        let p = dir.path().join(format!("{arg}.jsonl"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Collapses runs of whitespace to single spaces.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
