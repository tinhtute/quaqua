//! The claude driver: `--model claude-<model with dashes> --effort <effort>`.

use super::{exclude_from_git, sh_quote, write_atomic, HarnessDriver, Hook};
use std::io;
use std::path::Path;

pub struct Claude;

/// Relative to the worktree: untracked, per-worktree settings.
const HOOK_REL: &str = ".claude/settings.local.json";
/// Ends the command of the hook entry quaqua owns.
const MARKER: &str = "# qq-gate";

fn model_id(model: &str) -> String {
    if model.starts_with("claude-") {
        model.to_string()
    } else {
        format!("claude-{}", model.replace('.', "-"))
    }
}

impl HarnessDriver for Claude {
    fn harness(&self) -> &str {
        "claude"
    }

    fn herdr_kind(&self) -> &str {
        "claude"
    }

    fn launch_args(&self, model: &str, effort: Option<&str>) -> Vec<String> {
        let mut out = vec!["--model".to_string(), model_id(model)];
        if let Some(e) = effort {
            out.extend(["--effort".to_string(), e.to_string()]);
        }
        out
    }

    /// Merges a `PreToolUse` Bash hook running `<gate> gate` into
    /// `<worktree>/.claude/settings.local.json` (untracked, and excluded via
    /// the repo's `info/exclude`). Our entry is found by [`MARKER`] and
    /// replaced; other settings are kept and a repeat changes nothing. An
    /// existing file that is not valid settings is left untouched (`Err`).
    fn install_hook(&self, worktree: &Path, gate: &Path) -> io::Result<Option<Hook>> {
        use serde_json::{json, Value};
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
        let path = worktree.join(HOOK_REL);
        let mut root: Value = match std::fs::read_to_string(&path) {
            Ok(t) => serde_json::from_str(&t)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => json!({}),
            Err(e) => return Err(e),
        };
        // Any non-zero gate exit (usage error, crash, missing binary) maps to
        // exit 2, the only code that makes Claude block.
        let cmd = format!(
            "{} gate || exit 2 {MARKER}",
            sh_quote(&gate.to_string_lossy())
        );
        let entry = json!({"matcher": "Bash", "hooks": [{"type": "command", "command": cmd}]});
        let list = root
            .as_object_mut()
            .ok_or_else(|| bad("settings not an object"))?
            .entry("hooks")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| bad("hooks not an object"))?
            .entry("PreToolUse")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| bad("PreToolUse not a list"))?;
        let ours = |e: &Value| {
            e.pointer("/hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|h| {
                    h.iter().any(|x| {
                        x.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.ends_with(MARKER))
                    })
                })
        };
        let first = list.iter().position(ours);
        let mut i = 0;
        list.retain(|e| {
            i += 1;
            !ours(e) || Some(i - 1) == first
        });
        match list.iter().position(ours) {
            Some(p) => list[p] = entry,
            None => list.push(entry),
        }
        let mut text = serde_json::to_string_pretty(&root).map_err(io::Error::other)?;
        text.push('\n');
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            write_atomic(&path, text.as_bytes())?;
        }
        exclude_from_git(worktree, &format!("/{HOOK_REL}"))?;
        Ok(Some(Hook { path }))
    }
}
