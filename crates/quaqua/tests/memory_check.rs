//! `quaqua memory check` end-to-end: Unicode-char counting (not bytes), the
//! two caps from AGENTS.md '## Memory', the secret scan, and the fact that no
//! env var can raise a cap (R3).

use std::path::PathBuf;
use std::process::{Command, Output};

/// One scratch workspace: `memory/rules.md`, `USER.md`, and the `AGENTS.md`
/// marker a real workspace carries.
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
            "quaqua-memcheck-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("memory")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "# agents\n").unwrap();
        Ws { root }
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn run(&self) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
        cmd.args([
            "memory",
            "check",
            "--workspace",
            self.root.to_str().unwrap(),
        ])
        // A host workspace must never leak in through the default root.
        .env("QUAQUA_HOME", &self.root)
        .env_remove("HERDR_PANE_ID");
        cmd.output().expect("failed to spawn the quaqua binary")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn a_clean_workspace_exits_zero_and_says_nothing() {
    let ws = Ws::new("clean");
    ws.write("memory/rules.md", "# rules\n- one rule\n");
    ws.write("USER.md", "# anh iu\n- prefers terse reports\n");

    let out = ws.run();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "", "a clean check must be silent");
}

#[test]
fn missing_files_are_not_violations() {
    let ws = Ws::new("missing");
    let out = ws.run();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
}

#[test]
fn rules_md_over_the_cap_names_the_file_size_and_cap() {
    let ws = Ws::new("rules-over");
    ws.write("memory/rules.md", &"a".repeat(2201));

    let out = ws.run();
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("memory/rules.md"), "{text}");
    assert!(text.contains("2201 chars"), "{text}");
    assert!(text.contains("2200 cap"), "{text}");
}

#[test]
fn exactly_at_the_cap_is_clean() {
    let ws = Ws::new("rules-edge");
    ws.write("memory/rules.md", &"a".repeat(2200));
    ws.write("USER.md", &"b".repeat(1375));

    let out = ws.run();
    assert_eq!(out.status.code(), Some(0), "stdout: {}", stdout(&out));
}

#[test]
fn user_md_has_its_own_cap() {
    let ws = Ws::new("user-over");
    ws.write("USER.md", &"b".repeat(1376));

    let out = ws.run();
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("USER.md"), "{text}");
    assert!(text.contains("1376 chars"), "{text}");
    assert!(text.contains("1375 cap"), "{text}");
}

/// The cap counts Unicode chars: 2200 `ế` are 6600 bytes and still legal.
#[test]
fn the_cap_counts_chars_not_bytes() {
    let ws = Ws::new("unicode");
    let body = "ế".repeat(2200);
    assert_eq!(body.len(), 6600, "fixture is not multibyte");
    ws.write("memory/rules.md", &body);

    let out = ws.run();
    assert_eq!(out.status.code(), Some(0), "stdout: {}", stdout(&out));
}

#[test]
fn a_secret_in_either_file_is_a_violation() {
    for (rel, secret) in [
        (
            "memory/rules.md",
            "deploy key sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
        ),
        ("USER.md", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        ("USER.md", "-----BEGIN OPENSSH PRIVATE KEY-----"),
    ] {
        let ws = Ws::new("secret");
        ws.write(rel, &format!("# notes\n- {secret}\n"));
        let out = ws.run();
        assert_eq!(out.status.code(), Some(1), "{rel} / {secret}");
        assert!(stdout(&out).contains(rel), "{}", stdout(&out));
    }
}

/// R3: the caps are constants. An env var that looks like an override changes
/// nothing, because no code reads one.
#[test]
fn no_env_var_raises_a_cap() {
    let ws = Ws::new("no-override");
    ws.write("memory/rules.md", &"a".repeat(2201));

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.args(["memory", "check", "--workspace", ws.root.to_str().unwrap()])
        .env("QUAQUA_HOME", &ws.root)
        .env("QUAQUA_MEMORY_CAP", "99999")
        .env("QUAQUA_RULES_CAP", "99999")
        .env_remove("HERDR_PANE_ID");
    let out = cmd.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "an env var raised the cap: {}",
        stdout(&out)
    );
}

#[test]
fn a_missing_workspace_dir_is_an_error_not_a_pass() {
    let ws = Ws::new("gone");
    let missing = ws.root.join("nope");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.args(["memory", "check", "--workspace", missing.to_str().unwrap()])
        .env("QUAQUA_HOME", &ws.root)
        .env_remove("HERDR_PANE_ID");
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("is not a directory"),
        "{}",
        stderr(&out)
    );
}

/// The default workspace is the resolved workspace root, not the cwd.
#[test]
fn the_default_workspace_is_the_workspace_root() {
    let ws = Ws::new("default-root");
    ws.write("memory/rules.md", &"a".repeat(2201));

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.args(["memory", "check"])
        .env("QUAQUA_HOME", &ws.root)
        .current_dir(std::env::temp_dir())
        .env_remove("HERDR_PANE_ID");
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    assert!(stdout(&out).contains("memory/rules.md"), "{}", stdout(&out));
}
