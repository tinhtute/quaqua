//! The CoS code fence: "CoS never edits code or config" (AGENTS.md
//! '## Coordinator Rules') as a `PreToolUse` check.
//!
//! The hook learns its caller from its own input only: the CoS session runs in
//! the workspace root (`projects/INDEX.md` beside `cwd`, never a worker
//! worktree). Any other cwd — a worker, a Lead, an unknown caller — is not
//! fenced. The fence only ever answers `ask`, and every input it cannot read
//! falls through to the cap logic.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::identity::is_eligible_workspace;

const FENCED_DIRS: &[&str] = &["crates", "tests", ".github"];
const REASON: &str =
    "CoS never edits code or config (AGENTS.md '## Coordinator Rules'): assign it to a worker";

/// `Some(ask)` when a CoS `Write|Edit|MultiEdit|NotebookEdit` targets code or a
/// git worktree; `None` otherwise.
pub fn cos_fence(doc: &Value) -> Option<Value> {
    let tool = doc.get("tool_name")?.as_str()?;
    if !matches!(tool, "Write" | "Edit" | "MultiEdit" | "NotebookEdit") {
        return None;
    }
    let cwd = Path::new(doc.get("cwd")?.as_str()?);
    let input = doc.get("tool_input")?;
    let target = input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))?
        .as_str()?;
    let target = Path::new(target);
    if !target.is_absolute()
        || !is_eligible_workspace(cwd, cwd)
        || !cwd.join("projects/INDEX.md").is_file()
    {
        return None;
    }
    if is_fenced(target, cwd) {
        return Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "ask",
                "permissionDecisionReason": REASON,
            }
        }));
    }
    None
}

fn same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Resolve symlinks in the deepest existing ancestor, keep the rest as given.
fn resolve(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    for a in path.ancestors() {
        if let Ok(base) = a.canonicalize() {
            let mut out = base;
            out.extend(tail.iter().rev());
            return out;
        }
        if let Some(n) = a.file_name() {
            tail.push(n.to_os_string());
        }
    }
    path.to_path_buf()
}

fn is_fenced(target: &Path, ws: &Path) -> bool {
    let target = resolve(target);
    // `..` would hide a worktree behind the workspace.
    if target
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let ws = resolve(ws);
    if target.starts_with(&ws) && !ws.join(".git").exists() {
        return false;
    }
    if target.starts_with("/tmp")
        && target
            .components()
            .nth(2)
            .is_some_and(|c| c.as_os_str().to_string_lossy().starts_with("claude-"))
    {
        return false;
    }
    // Nearest repo or worktree root above the target (`.git` dir or file).
    let Some(root) = target.ancestors().skip(1).find(|a| a.join(".git").exists()) else {
        return false;
    };
    if same(root, &ws) {
        // The workspace is itself a repo: only its code dirs are fenced.
        let rel = target.strip_prefix(root).unwrap_or(&target);
        return rel
            .components()
            .next()
            .is_some_and(|c| FENCED_DIRS.iter().any(|d| c.as_os_str() == *d));
    }
    true
}
