//! The CoS code fence: "CoS never edits code or config" (AGENTS.md
//! '## Work') as a `PreToolUse` check.
//!
//! The hook learns its caller from its own input only: the CoS session runs in
//! the workspace root (`projects/INDEX.md` beside `cwd`, never a worker
//! worktree). Any other cwd — a worker, a Lead, an unknown caller — is not
//! fenced. The fence answers `ask` (code) or `deny` (the two memory files), and every input it cannot read
//! falls through to the cap logic. It runs after the cap logic, so a cap or
//! threat `deny` always wins over the fence `ask`.
//!
//! One more rule, a `deny`: the CoS never writes `USER.md` or `memory/rules.md`
//! directly. Those go through the cos-memory tools (the daemon's `memory.write`
//! verb, which journals and guards drift). The verb writes as the daemon, not
//! through this hook, so it is not affected. The Bash half of the rule lives in
//! `authority/gate.rs`; both read the file list from [`super::TARGETS`].
//!
//! Known gaps, by design: the fence is advisory. `Bash` writes (`sed -i`,
//! `cat >`) are not seen, and a CoS whose cwd moved off the workspace root
//! (a `cd` into a worktree) is judged as a non-CoS. Conversely a worker or
//! Lead that runs `cd` into the workspace root is judged as the CoS, so its
//! own worktree writes get an `ask`.

use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use crate::identity::is_eligible_workspace;

const FENCED_DIRS: &[&str] = &["crates", "tests", ".github"];
const REASON: &str =
    "CoS never edits code or config (AGENTS.md '## Work'): assign it to a worker";

const MEMORY_REASON: &str = "USER.md and memory/rules.md are written only through the cos-memory tools (memory_add, memory_replace, memory_remove), never directly";

/// `Some(ask)` when a CoS `Write|Edit|MultiEdit|NotebookEdit` targets code or a
/// git worktree, `Some(deny)` when it targets a memory file; `None` otherwise.
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
    if !cwd.is_absolute()
        || !is_eligible_workspace(cwd, cwd)
        || !cwd.join("projects/INDEX.md").is_file()
    {
        return None;
    }
    // A relative target is relative to the payload cwd.
    let target = lexical(&cwd.join(target));
    if is_memory_file(&target, cwd) {
        return Some(decision("deny", MEMORY_REASON));
    }
    if is_fenced(&target, cwd) {
        return Some(decision("ask", REASON));
    }
    None
}

fn decision(kind: &str, reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": kind,
            "permissionDecisionReason": reason,
        }
    })
}

/// True when `target` is one of the memory files under the workspace `ws`.
fn is_memory_file(target: &Path, ws: &Path) -> bool {
    let (target, ws) = (resolve(target), resolve(ws));
    super::TARGETS.iter().any(|t| target == ws.join(t.rel))
}

/// Drop `.` and fold `..` without touching the disk, so a `..` behind a
/// missing directory cannot hide a worktree.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
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
    let ws = resolve(ws);
    if target.starts_with(&ws) && !ws.join(".git").exists() {
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
