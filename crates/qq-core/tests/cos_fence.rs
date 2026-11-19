//! The CoS code fence in `memory hook pre`: ask on code, allow workspace files.

use std::fs;
use std::path::Path;

use qq_core::memory;
use serde_json::{json, Value};

fn call(tool: &str, cwd: &Path, path: &Path) -> String {
    json!({"tool_name": tool, "cwd": cwd, "tool_input": {"file_path": path, "content": "x"}})
        .to_string()
}

fn decision(v: &Option<Value>) -> Option<&str> {
    v.as_ref()?["hookSpecificOutput"]["permissionDecision"].as_str()
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.tmp);
    }
}

struct Fixture {
    tmp: std::path::PathBuf,
    ws: std::path::PathBuf,
    repo: std::path::PathBuf,
    wt: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = std::env::temp_dir().join(format!(
        "qq-cosfence-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let base = tmp.canonicalize().unwrap();
    let ws = base.join("quaqua");
    fs::create_dir_all(ws.join("projects")).unwrap();
    fs::create_dir_all(ws.join("memory")).unwrap();
    fs::write(ws.join("AGENTS.md"), "x").unwrap();
    fs::write(ws.join("projects/INDEX.md"), "x").unwrap();
    let repo = base.join("Lab/quaqua");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(repo.join("crates")).unwrap();
    let wt = base.join("Lab/.wt/task");
    fs::create_dir_all(&wt).unwrap();
    fs::write(wt.join(".git"), "gitdir: x").unwrap();
    Fixture {
        tmp,
        ws,
        repo,
        wt,
    }
}

#[test]
fn cos_fence_asks_on_code_and_worktrees() {
    let f = fixture();
    for tool in ["Write", "Edit", "MultiEdit"] {
        let r = memory::pre_decision(&call(tool, &f.ws, &f.repo.join("crates/a/src/lib.rs")));
        assert_eq!(decision(&r), Some("ask"), "{tool}");
        assert!(r.unwrap()["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("CoS never edits code or config"));
    }
    let nb = json!({"tool_name": "NotebookEdit", "cwd": f.ws,
        "tool_input": {"notebook_path": f.repo.join("tests/a.ipynb")}})
    .to_string();
    assert_eq!(decision(&memory::pre_decision(&nb)), Some("ask"));
    // inside a worktree, any file; a new file under a missing dir too
    assert_eq!(
        decision(&memory::pre_decision(&call(
            "Write",
            &f.ws,
            &f.wt.join("README.md")
        ))),
        Some("ask")
    );
    assert_eq!(
        decision(&memory::pre_decision(&call(
            "Write",
            &f.ws,
            &f.repo.join(".github/new/ci.yml")
        ))),
        Some("ask")
    );
}

#[test]
fn cos_fence_allows_workspace_files_and_scratchpad() {
    let f = fixture();
    for p in [
        f.ws.join("memory/2026-11-19.md"),
        f.ws.join("projects/x.md"),
        f.ws.join("raw/r.md"),
        Path::new("/tmp/claude-1000/scratch/a.md").to_path_buf(),
    ] {
        assert_eq!(
            decision(&memory::pre_decision(&call("Write", &f.ws, &p))),
            None,
            "{p:?}"
        );
    }
}

#[test]
fn cos_fence_leaves_workers_and_bad_input_alone() {
    let f = fixture();
    let code = f.repo.join("crates/a/src/lib.rs");
    // worker: cwd is its worktree
    assert_eq!(
        decision(&memory::pre_decision(&call("Write", &f.wt, &code))),
        None
    );
    // unknown caller: no cwd, or a cwd that is not the workspace
    let no_cwd = json!({"tool_name": "Write", "tool_input": {"file_path": code, "content": "x"}})
        .to_string();
    assert_eq!(decision(&memory::pre_decision(&no_cwd)), None);
    assert_eq!(
        decision(&memory::pre_decision(&call("Write", &f.repo, &code))),
        None
    );
    // parse failure
    assert_eq!(decision(&memory::pre_decision("{not json")), None);
    assert_eq!(decision(&memory::pre_decision("")), None);
}
