//! `quaqua memory` — the CoS memory caps and secret gate.
//!
//! AGENTS.md '## Memory' caps two files: `memory/rules.md` (durable rules) and
//! `USER.md` (what the Principal wants). The caps were convention only; this
//! module makes the engine enforce them.
//!
//! - `quaqua memory hook pre` is the `PreToolUse` (`Write|Edit|MultiEdit`) hook
//!   the workspace template installs: it simulates the post-edit content and denies the
//!   call (`hookSpecificOutput.permissionDecision = "deny"`) when the result
//!   would break a cap or carry a secret.
//! - `quaqua memory hook batch` is the `PostToolBatch` hook: it re-runs the
//!   check after a batch that could have written, so a `Bash`/`sed` write the
//!   file tools never saw is flagged in the same turn via `additionalContext`.
//!
//! The caps are constants here and nowhere else: no config key, CLI flag or
//! env var raises them (R3). Both hooks fail open — a hook that cannot parse
//! its input must never lock the agent out
//! of its own memory.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::threat::{self, Scope};

/// Cap on `memory/rules.md`, in Unicode chars (AGENTS.md '## Memory').
pub const RULES_CAP: usize = 2200;
/// Cap on `USER.md`, in Unicode chars (AGENTS.md '## Memory').
pub const USER_CAP: usize = 1375;

/// Cap on one captured line for a memory file that is not itself capped, in
/// Unicode chars — `memory/corrections.md` today. Such a line is cut, never
/// refused: a long prompt still leaves one readable correction.
pub const LINE_CAP: usize = 200;

/// One capped file: workspace-relative path and its cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub rel: &'static str,
    pub cap: usize,
}

/// The capped files. The only place a cap is written down.
pub const TARGETS: &[Target] = &[
    Target {
        rel: "memory/rules.md",
        cap: RULES_CAP,
    },
    Target {
        rel: "USER.md",
        cap: USER_CAP,
    },
];

/// What is wrong with a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Over the cap: `chars` Unicode chars against `cap`.
    OverCap {
        chars: usize,
        cap: usize,
        /// The first non-heading lines of the file, oldest first.
        oldest: Vec<String>,
    },
    /// A secret pattern matched, named for the message.
    Secret(&'static str),
    /// A threat pattern (or invisible codepoint) matched, by id.
    Threat(String),
}

/// One violation: a capped file and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub file: String,
    pub problem: Problem,
}

impl Problem {
    /// What is wrong, without the file: shared by the check line and the
    /// `scan` refusal, so the wording has one home.
    fn text(&self) -> String {
        match self {
            Problem::OverCap { chars, cap, oldest } => {
                let mut msg = format!("{chars} chars > {cap} cap — merge or drop entries");
                if !oldest.is_empty() {
                    let list: Vec<String> = oldest.iter().map(|l| format!("\"{l}\"")).collect();
                    msg.push_str(&format!("; oldest lines: {}", list.join(" | ")));
                }
                msg
            }
            Problem::Secret(name) => format!("contains a {name}"),
            Problem::Threat(id) => format!("matches threat pattern {id}"),
        }
    }
}

impl Violation {
    /// The one-line report, shared by `check`, the deny reason and the
    /// `additionalContext` text.
    pub fn line(&self) -> String {
        format!("{}: {}", self.file, self.problem.text())
    }
}

/// Check one file's text against its cap and the secret patterns.
pub fn check_text(rel: &str, text: &str) -> Vec<Violation> {
    let mut out = Vec::new();
    if let Some(t) = TARGETS.iter().find(|t| t.rel == rel) {
        let chars = text.chars().count();
        if chars > t.cap {
            out.push(Violation {
                file: rel.to_string(),
                problem: Problem::OverCap {
                    chars,
                    cap: t.cap,
                    oldest: oldest_lines(text),
                },
            });
        }
    }
    for name in scan_secrets(text) {
        out.push(Violation {
            file: rel.to_string(),
            problem: Problem::Secret(name),
        });
    }
    out
}

/// How many of the oldest lines an over-cap refusal names.
const OLDEST_N: usize = 3;
/// Chars kept of each named line.
const OLDEST_LEN: usize = 80;

/// The first OLDEST_N content lines (blank and heading lines skipped), each
/// cut to OLDEST_LEN chars: the entries to merge or drop first.
fn oldest_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .take(OLDEST_N)
        .map(|l| l.chars().take(OLDEST_LEN).collect())
        .collect()
}

/// Check the workspace's capped files. A missing file is not a violation.
pub fn check_workspace(root: &Path) -> Vec<Violation> {
    let mut out = Vec::new();
    for t in TARGETS {
        let Ok(text) = std::fs::read_to_string(root.join(t.rel)) else {
            continue;
        };
        out.extend(check_text(t.rel, &text));
    }
    out
}

/// Screen one line for a memory file: the bounded text, or the refusal reason.
///
/// A secret is refused outright — cutting a key in half still leaves a key.
/// Anything else is cut to [`LINE_CAP`] chars, so the caller cannot write an
/// unbounded line.
pub fn screen_line(text: &str) -> Result<String, String> {
    if let Some(name) = scan_secrets(text).first() {
        return Err(Problem::Secret(name).text());
    }
    Ok(text.chars().take(LINE_CAP).collect())
}

/// The `PreToolUse` decision for a `Write|Edit|MultiEdit` call: `Some(json)`
/// to deny or (CoS code fence) ask, `None` for "no decision". Fails open on any
/// input it cannot read. The cap/threat `deny` is decided first and wins over
/// the fence `ask`; every cap path that gives no decision lets the fence run.
pub fn pre_decision(input: &str) -> Option<Value> {
    let doc = serde_json::from_str::<Value>(input).ok()?;
    cap_decision(&doc).or_else(|| super::fence::cos_fence(&doc))
}

fn cap_decision(doc: &Value) -> Option<Value> {
    let tool = doc.get("tool_name")?.as_str()?;
    let tool_input = doc.get("tool_input")?;
    let path = Path::new(tool_input.get("file_path")?.as_str()?);
    let (rel, capped) = match target_for(path) {
        Some(t) => (t.rel, true),
        None => (standing_rel(path)?, false),
    };
    let current = std::fs::read_to_string(path).unwrap_or_default();
    // An edit we cannot simulate (stale `old_string`) is one Claude Code
    // rejects on its own: nothing is written, so there is nothing to gate.
    let next = simulate(tool, tool_input, &current)?;
    let mut violations = if capped {
        check_text(rel, &next)
    } else {
        Vec::new()
    };
    // Strict scan of the lines this edit adds: a line already in the file must
    // not lock every later edit out of it, nor excuse a second payload.
    for id in threat::scan(&threat::added_lines(&current, &next), Scope::Strict) {
        violations.push(Violation {
            file: rel.to_string(),
            problem: Problem::Threat(id),
        });
    }
    if violations.is_empty() {
        return None;
    }
    let reason = violations
        .iter()
        .map(Violation::line)
        .collect::<Vec<_>>()
        .join("; ");
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": format!("{reason} (AGENTS.md '## Memory')"),
        }
    }))
}

/// The `PostToolBatch` context after a batch that could have written: the
/// violations as `additionalContext`, so a `Bash`/`sed` write is flagged in
/// the same turn. `None` when clean or when the input cannot be read.
pub fn batch_decision(input: &str) -> Option<Value> {
    let doc = serde_json::from_str::<Value>(input).ok()?;
    if !batch_could_write(&doc) {
        return None;
    }
    let cwd = doc
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let violations = check_workspace(&workspace_root_from(&cwd));
    if violations.is_empty() {
        return None;
    }
    let mut context =
        String::from("memory cap violated (AGENTS.md '## Memory') — fix it before the next turn: ");
    context.push_str(
        &violations
            .iter()
            .map(Violation::line)
            .collect::<Vec<_>>()
            .join("; "),
    );
    // additionalContext is capped at 10k chars; a truncated message is worse
    // than a short one, but two files cannot reach it.
    if context.chars().count() > 9000 {
        context = context.chars().take(9000).collect();
    }
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolBatch",
            "additionalContext": context,
        }
    }))
}

/// True when some tool call in the batch could have written a file. `Bash`
/// counts: the whole point of the batch gate is the write path the file tools
/// never see.
fn batch_could_write(doc: &Value) -> bool {
    const WRITERS: &[&str] = &["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"];
    doc.get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls.iter().any(|c| {
                c.get("tool_name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| WRITERS.contains(&n))
            })
        })
        .unwrap_or(false)
}

/// The capped file `path` names, if it sits in a workspace. A workspace is
/// recognised by `AGENTS.md` or `.quaqua/` beside the capped file, so an
/// unrelated `USER.md` elsewhere on disk is left alone.
fn target_for(path: &Path) -> Option<&'static Target> {
    for t in TARGETS {
        if !path.ends_with(t.rel) {
            continue;
        }
        let depth = Path::new(t.rel).components().count();
        let ws = path.ancestors().nth(depth)?;
        if ws.join("AGENTS.md").is_file() || ws.join(".quaqua").is_dir() {
            return Some(t);
        }
    }
    None
}

/// The standing files without a cap: threat-scanned on write, not capped.
const UNCAPPED_STANDING: &[&str] = &["identity.md", "projects/INDEX.md"];

/// The uncapped standing file `path` names, if it sits in a workspace.
fn standing_rel(path: &Path) -> Option<&'static str> {
    let rel = UNCAPPED_STANDING.iter().find(|r| path.ends_with(r))?;
    let depth = Path::new(rel).components().count();
    let ws = path.ancestors().nth(depth)?;
    (ws.join("AGENTS.md").is_file() || ws.join(".quaqua").is_dir()).then_some(*rel)
}

/// The workspace `cwd` sits in: the nearest ancestor that carries `AGENTS.md`
/// or `.quaqua/`. A session that `cd`-ed into `memory/` or `src/` still gets
/// its batch checked against the workspace, not against the subdirectory.
fn workspace_root_from(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|a| a.join("AGENTS.md").is_file() || a.join(".quaqua").is_dir())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.to_path_buf())
}

/// The content a `Write`/`Edit`/`MultiEdit` call would leave behind, or `None`
/// when the call cannot be simulated.
fn simulate(tool: &str, input: &Value, current: &str) -> Option<String> {
    match tool {
        "Write" => input.get("content")?.as_str().map(str::to_string),
        "Edit" => apply_edit(
            current,
            input.get("old_string")?.as_str()?,
            input.get("new_string")?.as_str()?,
            input
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        "MultiEdit" => {
            let mut text = current.to_string();
            for edit in input.get("edits")?.as_array()? {
                text = apply_edit(
                    &text,
                    edit.get("old_string")?.as_str()?,
                    edit.get("new_string")?.as_str()?,
                    edit.get("replace_all")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                )?;
            }
            Some(text)
        }
        _ => None,
    }
}

/// Apply one `Edit` step. `None` when `old_string` is absent from the text: the
/// tool call fails on its own, so there is nothing to gate.
fn apply_edit(text: &str, old: &str, new: &str, replace_all: bool) -> Option<String> {
    if old.is_empty() {
        // `Edit` with an empty `old_string` creates the file.
        return Some(new.to_string());
    }
    if !text.contains(old) {
        return None;
    }
    Some(if replace_all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    })
}

// ── secret scan ────────────────────────────────────────────────────────────

/// Unambiguous token prefixes: `(prefix, min chars after it, what it is)`.
/// Longest first, so `sk-ant-` wins over `sk-`.
const TOKEN_PREFIXES: &[(&str, usize, &str)] = &[
    ("sk-ant-", 20, "Anthropic API key"),
    ("github_pat_", 20, "GitHub token"),
    ("ghp_", 20, "GitHub token"),
    ("gho_", 20, "GitHub token"),
    ("ghu_", 20, "GitHub token"),
    ("ghs_", 20, "GitHub token"),
    ("ghr_", 20, "GitHub token"),
    ("xoxb-", 10, "Slack token"),
    ("xoxp-", 10, "Slack token"),
    ("xoxa-", 10, "Slack token"),
    ("AIza", 30, "Google API key"),
    ("sk-", 20, "API key"),
];

/// `key: value` / `key=value` names whose value is a credential.
const SECRET_KEYS: &[&str] = &[
    "api_key",
    "api-key",
    "apikey",
    "access_key",
    "access_token",
    "auth_token",
    "secret_key",
    "secret-key",
    "private_key",
    "password",
    "passwd",
    "token",
    "secret",
];

/// Minimum length of a value that follows a `SECRET_KEYS` name before it counts
/// as a credential rather than prose.
const SECRET_VALUE_MIN: usize = 16;

/// Every secret pattern that matches `text`, named once each.
pub fn scan_secrets(text: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = Vec::new();
    let push = |found: &mut Vec<&'static str>, name: &'static str| {
        if !found.contains(&name) {
            found.push(name);
        }
    };

    for line in text.lines() {
        if line.contains("PRIVATE KEY-----") && line.contains("-----BEGIN") {
            push(&mut found, "private key");
        }
    }

    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        // A prefix only counts at a word boundary: `task-management-…` is not
        // an `sk-` key.
        if prev.is_none_or(|p| !is_word(p)) {
            let rest = &text[i..];
            for (prefix, min, name) in TOKEN_PREFIXES {
                if let Some(tail) = rest.strip_prefix(prefix) {
                    if token_run(tail) >= *min {
                        push(&mut found, name);
                    }
                    break;
                }
            }
            if rest
                .get(..4)
                .is_some_and(|s| s.eq_ignore_ascii_case("akia") || s.eq_ignore_ascii_case("asia"))
                && token_run(&rest[4..]) >= 16
            {
                push(&mut found, "AWS access key");
            }
            if rest
                .get(..7)
                .is_some_and(|s| s.eq_ignore_ascii_case("bearer "))
                && token_run(rest[7..].trim_start()) >= 20
            {
                push(&mut found, "bearer token");
            }
        }
        prev = Some(c);
    }

    // `key: value` / `key = value`. Lowercasing keeps the scan one pass and
    // byte offsets stay usable because only ASCII token chars are read back.
    let lower = text.to_lowercase();
    for key in SECRET_KEYS {
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(key) {
            let at = from + rel;
            from = at + key.len();
            if lower[..at].chars().next_back().is_some_and(is_word) {
                continue;
            }
            let after = lower[from..].trim_start_matches(is_blank);
            let after = match after.strip_prefix(':').or_else(|| after.strip_prefix('=')) {
                Some(rest) => rest.trim_start_matches(is_blank),
                None => continue,
            };
            let after = after.trim_start_matches(['"', '\'', '`']);
            if after.chars().take_while(|c| is_token_char(*c)).count() >= SECRET_VALUE_MIN {
                push(&mut found, "credential value");
            }
        }
    }

    found
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=')
}

fn token_run(s: &str) -> usize {
    s.chars().take_while(|c| is_token_char(*c)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules.md seed text, read from init/seeds.rs (not edited there).
    fn seed_rules() -> String {
        let src = include_str!("../init/seeds.rs");
        let from = src.find("\"memory/rules.md\"").expect("rules seed");
        let start = from + src[from..].find("r#\"").expect("raw start") + 3;
        let end = start + src[start..].find("\"#").expect("raw end");
        src[start..end].to_string()
    }

    #[test]
    fn over_cap_refusal_names_the_oldest_rule_bullets_of_the_seed() {
        let mut text = seed_rules();
        text.push_str(&format!("- {}\n", "x".repeat(RULES_CAP)));
        let v = check_text("memory/rules.md", &text);
        let line = v[0].line();
        assert!(line.contains("oldest lines"), "{line}");
        for kept in ["Advisor workers", "Clean up each worker", "Ownership"] {
            assert!(line.contains(kept), "{line}");
        }
        for skipped in [
            "Durable lessons",
            "Pane layout",
            "Add rules here",
            "Gardener",
        ] {
            assert!(!line.contains(skipped), "{line}");
        }
    }

    #[test]
    fn oldest_lines_skips_comment_block_and_prose() {
        let t = "# H\nprose\n<!-- a\n- not a rule\n-->\n<!-- one -->\n- r1\n- r2\n";
        assert_eq!(oldest_lines(t), vec!["- r1", "- r2"]);
    }
}
