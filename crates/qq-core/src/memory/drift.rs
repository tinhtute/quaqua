//! Stale-session guard (issue #72): a Claude Code session records the rule
//! files it started with; the PreToolUse gate warns once when they change
//! before the session spawns or prompts. Warn and re-read, never block.

use crate::init::md5_hex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files read again after a compaction (memory/rules.md is re-read only, not
/// tracked for drift).
const REREAD: &[&str] = &["AGENTS.md", "memory/rules.md", "projects/INDEX.md"];

/// Tracked rule files under `root`, as sorted relative paths: team.toml,
/// AGENTS.md and every `rules/_all/*.md`. Missing files are left out.
pub fn tracked(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = ["team.toml", "AGENTS.md"]
        .iter()
        .filter(|f| root.join(f).is_file())
        .map(|f| f.to_string())
        .collect();
    if let Ok(rd) = std::fs::read_dir(root.join("rules/_all")) {
        let mut rules: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".md"))
            .map(|n| format!("rules/_all/{n}"))
            .collect();
        rules.sort();
        out.extend(rules);
    }
    out
}

pub fn snapshot(root: &Path) -> BTreeMap<String, String> {
    tracked(root)
        .into_iter()
        .filter_map(|rel| {
            let data = std::fs::read(root.join(&rel)).ok()?;
            Some((rel, md5_hex(&data)))
        })
        .collect()
}

/// `runs/.sessions/<id>.json`; `None` for an id that could escape the dir.
pub fn baseline_path(root: &Path, session_id: &str) -> Option<PathBuf> {
    let ok = !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| {
        root.join("runs/.sessions")
            .join(format!("{session_id}.json"))
    })
}

fn write_baseline(path: &Path, snap: &BTreeMap<String, String>) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(body) = serde_json::to_string(snap) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn read_baseline(path: &Path) -> Option<BTreeMap<String, String>> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// SessionStart: record the baseline (a `resume` keeps an existing one) and,
/// after a compaction, return the re-read list for stdout.
pub fn session_start(root: &Path, session_id: &str, source: &str) -> Option<String> {
    let path = baseline_path(root, session_id)?;
    if !(source == "resume" && path.is_file()) {
        write_baseline(&path, &snapshot(root));
    }
    (source == "compact").then(|| reread_list(root))
}

fn reread_list(root: &Path) -> String {
    let mut files: Vec<String> = REREAD
        .iter()
        .filter(|f| root.join(f).is_file())
        .map(|f| f.to_string())
        .collect();
    if let Some(audit) = newest_audit(root) {
        files.push(audit);
    }
    let list: Vec<String> = files
        .iter()
        .map(|f| format!("- {}", root.join(f).display()))
        .collect();
    format!(
        "Context was compacted: instructions are now a summary. Re-read before acting:\n{}",
        list.join("\n")
    )
}

fn newest_audit(root: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("reports"))
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("audit-") && n.ends_with(".md"))
        .collect();
    names.sort();
    names.pop().map(|n| format!("reports/{n}"))
}

/// Files added, changed or removed since the baseline; on drift the baseline is
/// refreshed so each change warns once. No baseline → nothing to compare.
pub fn drift(root: &Path, session_id: &str) -> Vec<String> {
    let Some(path) = baseline_path(root, session_id) else {
        return Vec::new();
    };
    let Some(old) = read_baseline(&path) else {
        return Vec::new();
    };
    let now = snapshot(root);
    let mut changed: Vec<String> = now
        .iter()
        .filter(|(k, v)| old.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .chain(old.keys().filter(|k| !now.contains_key(*k)).cloned())
        .collect();
    changed.sort();
    if !changed.is_empty() {
        write_baseline(&path, &now);
    }
    changed
}

/// True when any segment runs `quaqua|qq worker …` or `quaqua|qq lead …`.
pub fn is_dispatch(segs: &[Vec<String>]) -> bool {
    let bin =
        |w: &str| matches!(w, "quaqua" | "qq") || w.ends_with("/quaqua") || w.ends_with("/qq");
    segs.iter().any(|seg| {
        seg.iter()
            .position(|w| bin(w))
            .is_some_and(|i| matches!(seg.get(i + 1).map(String::as_str), Some("worker" | "lead")))
    })
}

/// The warning text for the rule files in `changed`.
pub fn drift_message(changed: &[String]) -> String {
    format!(
        "Rule files changed since this session started: {}. Re-read them before this spawn/prompt; your loaded rules may be stale (issue #72).",
        changed.join(", ")
    )
}
