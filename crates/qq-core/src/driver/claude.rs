//! The claude driver: `--model claude-<model with dashes> --effort <effort>`.

use super::{HarnessDriver, Hook};
use std::io;
use std::path::Path;

pub struct Claude;

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

    /// Merges a `PreToolUse` Bash hook calling `gate` into
    /// `<worktree>/.claude/settings.json`; other settings are kept and a
    /// repeat changes nothing.
    fn install_hook(&self, worktree: &Path, gate: &Path) -> io::Result<Option<Hook>> {
        use serde_json::{json, Value};
        let path = worktree.join(".claude/settings.json");
        let mut root: Value = match std::fs::read_to_string(&path) {
            Ok(t) => serde_json::from_str(&t)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => json!({}),
            Err(e) => return Err(e),
        };
        let cmd = gate.to_string_lossy().to_string();
        let entry = json!({"matcher": "Bash", "hooks": [{"type": "command", "command": cmd}]});
        let list = root
            .as_object_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "settings not an object"))?
            .entry("hooks")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "hooks not an object"))?
            .entry("PreToolUse")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "PreToolUse not a list"))?;
        if !list.contains(&entry) {
            list.push(entry);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(
                &tmp,
                serde_json::to_string_pretty(&root).unwrap_or_default(),
            )?;
            std::fs::rename(&tmp, &path)?;
        }
        Ok(Some(Hook { path }))
    }
}
