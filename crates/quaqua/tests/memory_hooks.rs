//! The two Claude Code hooks `quaqua init` installs, driven through the built
//! binary with real hook JSON on stdin: the `PreToolUse` deny for a write that
//! would break a memory cap or carry a secret, the `PostToolBatch` flag for a
//! write the file tools never saw, and the idempotent install into
//! `.claude/settings.json`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "common/gitfix.rs"]
mod gitfix;

/// A scratch workspace with the `AGENTS.md` marker the gate looks for.
struct Ws {
    root: PathBuf,
}

impl Ws {
    fn new(tag: &str) -> Self {
        static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "quaqua-memhook-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("memory")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "# agents\n").unwrap();
        Ws { root }
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Run `quaqua memory hook <sub>` with `payload` on stdin.
    fn hook(&self, sub: &str, payload: &str) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
        cmd.args(["memory", "hook", sub])
            .env("QUAQUA_HOME", &self.root)
            .env_remove("HERDR_PANE_ID")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().expect("spawn quaqua");
        {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(payload.as_bytes())
                .unwrap();
        }
        child.wait_with_output().unwrap()
    }

    fn pre(&self, payload: &str) -> Output {
        self.hook("pre", payload)
    }

    fn batch(&self, payload: &str) -> Output {
        self.hook("batch", payload)
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// The `PreToolUse` payload Claude Code sends for a file tool.
fn pre_payload(tool: &str, input: serde_json::Value) -> String {
    serde_json::json!({
        "session_id": "s1",
        "cwd": "/tmp",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": input,
        "tool_use_id": "toolu_01",
    })
    .to_string()
}

fn deny_reason(out: &Output) -> Option<String> {
    let doc: serde_json::Value = serde_json::from_str(&stdout(out)).ok()?;
    let specific = doc.get("hookSpecificOutput")?;
    assert_eq!(
        specific.get("hookEventName").and_then(|v| v.as_str()),
        Some("PreToolUse")
    );
    assert_eq!(
        specific.get("permissionDecision").and_then(|v| v.as_str()),
        Some("deny"),
        "not a deny: {}",
        stdout(out)
    );
    specific
        .get("permissionDecisionReason")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn additional_context(out: &Output) -> Option<String> {
    let doc: serde_json::Value = serde_json::from_str(&stdout(out)).ok()?;
    let specific = doc.get("hookSpecificOutput")?;
    assert_eq!(
        specific.get("hookEventName").and_then(|v| v.as_str()),
        Some("PostToolBatch")
    );
    specific
        .get("additionalContext")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

// ── PreToolUse ─────────────────────────────────────────────────────────────

#[test]
fn a_write_that_breaks_the_cap_is_denied_with_size_and_cap() {
    let ws = Ws::new("write-over");
    let payload = pre_payload(
        "Write",
        serde_json::json!({
            "file_path": ws.path("memory/rules.md"),
            "content": "a".repeat(2201),
        }),
    );

    let out = ws.pre(&payload);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let reason = deny_reason(&out).expect("no deny emitted");
    assert!(reason.contains("memory/rules.md"), "{reason}");
    assert!(reason.contains("2201 chars"), "{reason}");
    assert!(reason.contains("2200 cap"), "{reason}");
}

#[test]
fn a_write_at_the_cap_is_allowed() {
    let ws = Ws::new("write-edge");
    let payload = pre_payload(
        "Write",
        serde_json::json!({
            "file_path": ws.path("memory/rules.md"),
            "content": "a".repeat(2200),
        }),
    );

    let out = ws.pre(&payload);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "", "a legal write must stay silent");
}

#[test]
fn a_write_carrying_a_secret_is_denied() {
    let ws = Ws::new("write-secret");
    let payload = pre_payload(
        "Write",
        serde_json::json!({
            "file_path": ws.path("USER.md"),
            "content": "# anh iu\n- token: sk-ant-api03-abcdefghijklmnopqrstuvwxyz\n",
        }),
    );

    let out = ws.pre(&payload);
    let reason = deny_reason(&out).expect("no deny emitted");
    assert!(reason.contains("USER.md"), "{reason}");
    assert!(reason.contains("Anthropic API key"), "{reason}");
}

/// The gate simulates the edit: a small insertion into a file already near the
/// cap is denied, the same insertion into a short file is not.
#[test]
fn an_edit_is_denied_only_when_the_result_breaks_the_cap() {
    let ws = Ws::new("edit-grow");
    ws.write("memory/rules.md", &"a".repeat(2195));
    let payload = pre_payload(
        "Edit",
        serde_json::json!({
            "file_path": ws.path("memory/rules.md"),
            "old_string": "a",
            "new_string": "a".repeat(20),
        }),
    );
    let out = ws.pre(&payload);
    let reason = deny_reason(&out).expect("no deny emitted");
    assert!(reason.contains("2214 chars"), "{reason}");

    let ws = Ws::new("edit-shrink");
    ws.write("memory/rules.md", &"a".repeat(2195));
    let payload = pre_payload(
        "Edit",
        serde_json::json!({
            "file_path": ws.path("memory/rules.md"),
            "old_string": "a".repeat(10),
            "new_string": "a",
        }),
    );
    let out = ws.pre(&payload);
    assert_eq!(stdout(&out), "", "a shrinking edit must stay silent");
}

#[test]
fn a_multiedit_is_simulated_as_a_whole() {
    let ws = Ws::new("multiedit");
    ws.write("memory/rules.md", &"a".repeat(2190));
    let payload = pre_payload(
        "MultiEdit",
        serde_json::json!({
            "file_path": ws.path("memory/rules.md"),
            "edits": [
                { "old_string": "a", "new_string": "b" },
                { "old_string": "a", "new_string": "c".repeat(40) },
            ],
        }),
    );

    let out = ws.pre(&payload);
    let reason = deny_reason(&out).expect("no deny emitted");
    assert!(reason.contains("2229 chars"), "{reason}");
}

#[test]
fn a_write_outside_the_capped_files_is_ignored() {
    let ws = Ws::new("other-file");
    let payload = pre_payload(
        "Write",
        serde_json::json!({
            "file_path": ws.path("src/main.rs"),
            "content": "a".repeat(5000),
        }),
    );
    let out = ws.pre(&payload);
    assert_eq!(stdout(&out), "");
}

/// A `USER.md` with no workspace around it is someone else's file.
#[test]
fn a_user_md_outside_a_workspace_is_ignored() {
    let ws = Ws::new("stray-user");
    let stray = ws.root.join("elsewhere/USER.md");
    std::fs::create_dir_all(stray.parent().unwrap()).unwrap();
    let payload = pre_payload(
        "Write",
        serde_json::json!({
            "file_path": stray,
            "content": "a".repeat(5000),
        }),
    );
    let out = ws.pre(&payload);
    assert_eq!(stdout(&out), "");
}

#[test]
fn unparseable_hook_input_fails_open() {
    let ws = Ws::new("garbage");
    let out = ws.pre("not json at all");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "");
}

// ── PostToolBatch ──────────────────────────────────────────────────────────

fn batch_payload(cwd: &Path, tools: &[&str]) -> String {
    let calls: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "tool_name": t,
                "tool_input": { "command": "sed -i s/a/b/ memory/rules.md" },
                "tool_use_id": "toolu_01",
                "tool_response": "ok",
            })
        })
        .collect();
    serde_json::json!({
        "session_id": "s1",
        "cwd": cwd,
        "hook_event_name": "PostToolBatch",
        "tool_calls": calls,
    })
    .to_string()
}

#[test]
fn a_bash_write_that_left_the_cap_broken_is_flagged() {
    let ws = Ws::new("batch-over");
    ws.write("memory/rules.md", &"a".repeat(2300));

    let out = ws.batch(&batch_payload(&ws.root, &["Bash"]));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let context = additional_context(&out).expect("no additionalContext emitted");
    assert!(context.contains("memory/rules.md"), "{context}");
    assert!(context.contains("2300 chars"), "{context}");
    assert!(context.contains("2200 cap"), "{context}");
}

/// A session that `cd`-ed into a subdirectory still gets the workspace checked.
#[test]
fn a_batch_in_a_subdirectory_still_finds_the_workspace() {
    let ws = Ws::new("batch-subdir");
    ws.write("memory/rules.md", &"a".repeat(2300));

    let out = ws.batch(&batch_payload(&ws.path("src"), &["Bash"]));
    let context = additional_context(&out).expect("no additionalContext emitted");
    assert!(context.contains("memory/rules.md"), "{context}");
}

#[test]
fn a_clean_workspace_gets_no_context() {
    let ws = Ws::new("batch-clean");
    ws.write("memory/rules.md", "# rules\n");

    let out = ws.batch(&batch_payload(&ws.root, &["Bash"]));
    assert_eq!(stdout(&out), "");
}

/// A batch of reads cannot have written anything, so it does not nag.
#[test]
fn a_read_only_batch_gets_no_context() {
    let ws = Ws::new("batch-read");
    ws.write("memory/rules.md", &"a".repeat(2300));

    let out = ws.batch(&batch_payload(&ws.root, &["Read", "Grep"]));
    assert_eq!(stdout(&out), "");
}

// ── `quaqua init` installs the hooks ───────────────────────────────────────

/// A template fixture: the smallest tree `init` accepts.
fn template_fixture(root: &Path) -> PathBuf {
    let tpl = root.join("tpl");
    std::fs::create_dir_all(tpl.join("workspace")).unwrap();
    std::fs::create_dir_all(tpl.join("seed")).unwrap();
    std::fs::write(tpl.join("workspace/AGENTS.md"), "# agents\n").unwrap();
    std::fs::write(tpl.join("seed/memory-rules.md"), "# rules\n").unwrap();
    tpl
}

fn init_cmd(tpl: &Path, root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.env("QUAQUA_TEMPLATE_DIR", tpl)
        .env("PATH", root.join("nobin"))
        .env("HERDR_BIN_PATH", root.join("nobin/herdr"))
        .env_remove("HERDR_PANE_ID")
        .env_remove("QUAQUA_HOME")
        .current_dir(root);
    gitfix::isolate(&mut cmd);
    cmd
}

fn settings_hook_commands(settings: &str, event: &str) -> Vec<String> {
    let doc: serde_json::Value = serde_json::from_str(settings).expect("settings is not JSON");
    doc["hooks"][event]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                .filter_map(|h| h["command"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn init_installs_both_memory_hooks_and_re_running_changes_nothing() {
    let ws = Ws::new("init-install");
    let tpl = template_fixture(&ws.root);
    let target = ws.root.join("ws");

    let out = init_cmd(&tpl, &ws.root)
        .args(["init", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed: {}", stderr(&out));

    let settings = std::fs::read_to_string(target.join(".claude/settings.json")).unwrap();
    let pre = settings_hook_commands(&settings, "PreToolUse");
    let batch = settings_hook_commands(&settings, "PostToolBatch");
    assert!(
        pre.iter().any(|c| c.contains("memory hook pre")),
        "no PreToolUse memory hook: {settings}"
    );
    assert!(
        batch.iter().any(|c| c.contains("memory hook batch")),
        "no PostToolBatch memory hook: {settings}"
    );
    // The matcher is what routes Write/Edit/MultiEdit to the gate.
    let doc: serde_json::Value = serde_json::from_str(&settings).unwrap();
    assert!(doc["hooks"]["PreToolUse"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["matcher"] == "Write|Edit|MultiEdit"));

    // Idempotent: a second init writes the same bytes and adds no handler.
    let out = init_cmd(&tpl, &ws.root)
        .args(["init", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "re-run failed: {}", stderr(&out));
    let again = std::fs::read_to_string(target.join(".claude/settings.json")).unwrap();
    assert_eq!(again, settings, "a re-run rewrote settings.json");
    assert_eq!(
        settings_hook_commands(&again, "PreToolUse")
            .iter()
            .filter(|c| c.contains("memory hook pre"))
            .count(),
        1,
        "the hook was installed twice: {again}"
    );
}

#[test]
fn init_keeps_the_hooks_a_workspace_already_declares() {
    let ws = Ws::new("init-merge");
    let tpl = template_fixture(&ws.root);
    let target = ws.root.join("ws");
    std::fs::create_dir_all(target.join(".claude")).unwrap();
    std::fs::write(
        target.join(".claude/settings.json"),
        r#"{
  "permissions": { "deny": ["Agent"] },
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash", "hooks": [ { "type": "command", "command": "mine.sh" } ] }
    ]
  }
}
"#,
    )
    .unwrap();

    let out = init_cmd(&tpl, &ws.root)
        .args(["init", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed: {}", stderr(&out));

    let settings = std::fs::read_to_string(target.join(".claude/settings.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(
        doc["permissions"]["deny"][0], "Agent",
        "clobbered permissions"
    );
    let pre = settings_hook_commands(&settings, "PreToolUse");
    assert!(
        pre.iter().any(|c| c == "mine.sh"),
        "dropped the user's hook"
    );
    assert!(
        pre.iter().any(|c| c.contains("memory hook pre")),
        "memory hook not added beside the user's: {settings}"
    );
}

/// `--update` reconciles the merged file instead of calling it a conflict: the
/// lock records what quaqua wrote, so the next update lands on the "untouched
/// locally" row and converges to the same bytes.
#[test]
fn update_keeps_the_memory_hooks_without_a_conflict() {
    let ws = Ws::new("init-update");
    let tpl = template_fixture(&ws.root);
    let target = ws.root.join("ws");

    let out = init_cmd(&tpl, &ws.root)
        .args(["init", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    let before = std::fs::read_to_string(target.join(".claude/settings.json")).unwrap();

    for _ in 0..2 {
        let out = init_cmd(&tpl, &ws.root)
            .args(["init", "--update", target.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "update reported a conflict: {}",
            stdout(&out)
        );
        assert!(
            !stdout(&out).contains("conflict .claude/settings.json"),
            "{}",
            stdout(&out)
        );
        let after = std::fs::read_to_string(target.join(".claude/settings.json")).unwrap();
        assert_eq!(after, before, "update drifted settings.json");
    }
}

/// The shipped settings template and the installer must name the same two
/// commands: the template is what a fresh workspace gets, the constants are
/// what `init` heals an existing workspace with. If they drift, the merge
/// rewrites a freshly booted file and the template test suite fails.
#[test]
fn the_shipped_template_carries_the_installed_commands() {
    let template = include_str!("../../../templates/workspace/.claude/settings.json");
    let pre = settings_hook_commands(template, "PreToolUse");
    let batch = settings_hook_commands(template, "PostToolBatch");
    assert!(
        pre.iter().any(|c| c == quaqua::memory::HOOK_PRE_COMMAND),
        "template PreToolUse does not carry HOOK_PRE_COMMAND: {template}"
    );
    assert!(
        batch
            .iter()
            .any(|c| c == quaqua::memory::HOOK_BATCH_COMMAND),
        "template PostToolBatch does not carry HOOK_BATCH_COMMAND: {template}"
    );
    let doc: serde_json::Value = serde_json::from_str(template).unwrap();
    assert!(doc["hooks"]["PreToolUse"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["matcher"] == quaqua::memory::HOOK_PRE_MATCHER));
}

#[test]
fn a_dry_run_installs_nothing() {
    let ws = Ws::new("init-dry");
    let tpl = template_fixture(&ws.root);
    let target = ws.root.join("ws");

    let out = init_cmd(&tpl, &ws.root)
        .args(["init", "--dry-run", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "dry run failed: {}", stderr(&out));
    assert!(!target.join(".claude").exists(), "dry run wrote .claude/");
}
