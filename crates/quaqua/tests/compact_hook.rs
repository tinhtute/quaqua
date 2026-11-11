//! The pre-compaction hooks `quaqua init` installs (G4): a `PreCompact` handler
//! and a `SessionStart` handler running `scripts/hooks/memory-compact.sh`, added
//! idempotently beside the hooks a workspace already declares.
//!
//! What the script itself does is `tests/test_memory_compact.sh`; this file only
//! owns the install contract (R7).
//!
//! Every test name carries `compact_hook`: the phase's acceptance command is
//! `cargo test -p quaqua compact_hook`, which filters test *names*, not the
//! test file — a name without the token would silently drop out of the gate.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "common/gitfix.rs"]
mod gitfix;

/// A scratch root holding the template fixture and whatever target a test asks
/// for.
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
            "quaqua-compacthook-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Ws { root }
    }

    /// The smallest tree `init` accepts.
    fn template(&self) -> PathBuf {
        let tpl = self.root.join("tpl");
        std::fs::create_dir_all(tpl.join("workspace")).unwrap();
        std::fs::create_dir_all(tpl.join("seed")).unwrap();
        std::fs::write(tpl.join("workspace/AGENTS.md"), "# agents\n").unwrap();
        std::fs::write(tpl.join("seed/memory-rules.md"), "# rules\n").unwrap();
        tpl
    }

    fn init(&self, tpl: &Path, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
        cmd.args(["init"])
            .args(args)
            .env("QUAQUA_TEMPLATE_DIR", tpl)
            .env("PATH", self.root.join("nobin"))
            .env("HERDR_BIN_PATH", self.root.join("nobin/herdr"))
            .env_remove("HERDR_PANE_ID")
            .env_remove("QUAQUA_HOME")
            .current_dir(&self.root);
        gitfix::isolate(&mut cmd);
        cmd.output().expect("run quaqua init")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Every command handler registered for `event`, in file order.
fn hook_commands(settings: &str, event: &str) -> Vec<String> {
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

fn count_containing(commands: &[String], needle: &str) -> usize {
    commands.iter().filter(|c| c.contains(needle)).count()
}

/// The settings a `quaqua init` workspace already has: the herdr-gate
/// SessionStart handler the template ships.
const HERDR_GATE: &str = "\"$CLAUDE_PROJECT_DIR\"/scripts/hooks/herdr-gate.sh --session-start";

fn settings_with_herdr_gate() -> String {
    format!(
        r#"{{
  "permissions": {{ "deny": ["Agent"] }},
  "hooks": {{
    "SessionStart": [
      {{ "hooks": [ {{ "type": "command", "command": {gate:?} }} ] }}
    ]
  }}
}}
"#,
        gate = HERDR_GATE
    )
}

#[test]
fn compact_hook_init_installs_beside_the_herdr_gate() {
    let ws = Ws::new("install");
    let tpl = ws.template();
    let target = ws.root.join("ws");
    std::fs::create_dir_all(target.join(".claude")).unwrap();
    std::fs::write(
        target.join(".claude/settings.json"),
        settings_with_herdr_gate(),
    )
    .unwrap();

    let out = ws.init(&tpl, &[target.to_str().unwrap()]);
    assert!(out.status.success(), "init failed: {}", stderr(&out));

    let settings = std::fs::read_to_string(target.join(".claude/settings.json")).unwrap();
    let start = hook_commands(&settings, "SessionStart");
    assert_eq!(
        start.first().map(String::as_str),
        Some(HERDR_GATE),
        "the herdr-gate handler moved or vanished: {settings}"
    );
    assert_eq!(
        count_containing(&start, "memory-compact.sh\" --session-start"),
        1,
        "no SessionStart snapshot handler: {settings}"
    );
    assert_eq!(
        count_containing(
            &hook_commands(&settings, "PreCompact"),
            "memory-compact.sh\" --pre-compact"
        ),
        1,
        "no PreCompact snapshot handler: {settings}"
    );
    // The install must not disturb the rest of the file.
    let doc: serde_json::Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(doc["permissions"]["deny"][0], "Agent");
}

#[test]
fn compact_hook_init_is_idempotent() {
    let ws = Ws::new("idempotent");
    let tpl = ws.template();
    let target = ws.root.join("ws");

    let out = ws.init(&tpl, &[target.to_str().unwrap()]);
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    let first = std::fs::read_to_string(target.join(".claude/settings.json")).unwrap();

    let out = ws.init(&tpl, &[target.to_str().unwrap()]);
    assert!(out.status.success(), "re-run failed: {}", stderr(&out));
    let again = std::fs::read_to_string(target.join(".claude/settings.json")).unwrap();

    assert_eq!(again, first, "a re-run rewrote settings.json");
    assert_eq!(
        count_containing(
            &hook_commands(&again, "PreCompact"),
            "memory-compact.sh\" --pre-compact"
        ),
        1,
        "the PreCompact hook was installed twice: {again}"
    );
    assert_eq!(
        count_containing(
            &hook_commands(&again, "SessionStart"),
            "memory-compact.sh\" --session-start"
        ),
        1,
        "the SessionStart hook was installed twice: {again}"
    );
}

#[test]
fn compact_hook_dry_run_writes_nothing() {
    let ws = Ws::new("dry-run");
    let tpl = ws.template();
    let target = ws.root.join("ws");

    let out = ws.init(&tpl, &["--dry-run", target.to_str().unwrap()]);
    assert!(out.status.success(), "dry run failed: {}", stderr(&out));
    assert!(
        stdout(&out).contains("would install memory hooks"),
        "dry run did not report the hooks: {}",
        stdout(&out)
    );
    assert!(
        !target.join(".claude/settings.json").exists(),
        "dry run wrote settings.json"
    );
}

#[test]
fn compact_hook_leaves_a_non_object_settings_file_untouched() {
    let ws = Ws::new("not-object");
    let tpl = ws.template();
    let target = ws.root.join("ws");
    std::fs::create_dir_all(target.join(".claude")).unwrap();
    let path = target.join(".claude/settings.json");
    std::fs::write(&path, "[]\n").unwrap();

    let out = ws.init(&tpl, &[target.to_str().unwrap()]);
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[]\n",
        "init rewrote a settings file it does not own"
    );
    assert!(
        stderr(&out).contains("is not a JSON object"),
        "the skip was silent: {}",
        stderr(&out)
    );
}

/// The shipped template and the installer must name the same two commands: the
/// template is what a fresh workspace gets, the constants are what `init` heals
/// an existing workspace with. Drift makes `--update` rewrite a fresh file.
#[test]
fn compact_hook_template_matches_the_installed_commands() {
    let template = include_str!("../../../templates/workspace/.claude/settings.json");
    let pre = hook_commands(template, "PreCompact");
    let start = hook_commands(template, "SessionStart");
    assert!(
        pre.iter().any(|c| c == quaqua::init::COMPACT_PRE_COMMAND),
        "template PreCompact does not carry COMPACT_PRE_COMMAND: {template}"
    );
    assert!(
        start
            .iter()
            .any(|c| c == quaqua::init::COMPACT_SESSION_COMMAND),
        "template SessionStart does not carry COMPACT_SESSION_COMMAND: {template}"
    );
    assert!(
        start.iter().any(|c| c == HERDR_GATE),
        "template dropped the herdr-gate handler: {template}"
    );
}
