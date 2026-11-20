//! T4 hook routes (hermes-cos R5, R7): the PreToolUse matchers in the workspace
//! template settings and in `install_hook` reach `quaqua gate`; the gate's MCP
//! arm is a floor action; the shell fallback denies the approval list when the
//! binary is missing.

use qq_core::authority::{gate_evaluate, gate_evaluate_in, GateVerdict};
use qq_core::cli::Env;
use qq_core::daemon::{Config, DaemonHandle};
use qq_core::driver::Registry;
use qq_core::herdr::{FakeHerdr, HerdrClient, Verb};
use qq_core::identity::peer::HerdrPeers;
use qq_core::testkit::TestWorkspace;
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Tool names every gate-routed output must hit.
const ROUTED: &[&str] = &[
    "Bash",
    "Write",
    "Edit",
    "MultiEdit",
    "mcp__claude_ai_Claude_Data__share_connector",
    "mcp__claude_ai_Claude_Data__share_live_queries",
    "mcp__plugin_tinyfish_tinyfish__run_deep_research",
    "mcp__plugin_tinyfish_tinyfish__run_deep_research_async",
    "mcp__plugin_tinyfish_tinyfish__run_web_automation",
    "mcp__plugin_tinyfish_tinyfish__run_web_automation_async",
    "mcp__plugin_tinyfish_tinyfish__create_dataset",
    "mcp__plugin_tinyfish_tinyfish__populate_dataset",
    "mcp__plugin_tinyfish_tinyfish__top_up_wallet",
    "mcp__claude_ai_Slack__send_message",
];

/// Tool names no gate-routed entry may hit.
const UNROUTED: &[&str] = &[
    "Read",
    "Grep",
    "mcp__claude_ai_Meetings__get_meeting",
    "mcp__plugin_tinyfish_tinyfish__search",
    "mcp__cos-memory__memory_add",
    "mcp__claude_ai_Claude_Data__unshare_connector",
];

fn template_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/workspace")
}

/// Claude Code reads a matcher of only letters, digits, `_` and `|` as an exact
/// name list, anything else as a regex.
fn matcher_hits(matcher: &str, name: &str) -> bool {
    let plain = matcher
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '|');
    let re = if plain {
        format!("^({matcher})$")
    } else {
        matcher.to_string()
    };
    regex::Regex::new(&re).unwrap().is_match(name)
}

/// The commands of every PreToolUse entry whose matcher hits `name`.
fn commands_for(settings: &Value, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for e in settings["hooks"]["PreToolUse"].as_array().unwrap() {
        let m = e["matcher"].as_str().unwrap_or("");
        if !matcher_hits(m, name) {
            continue;
        }
        for h in e["hooks"].as_array().unwrap() {
            out.push(h["command"].as_str().unwrap().to_string());
        }
    }
    out
}

/// A stand-in quaqua binary that logs its argv and drains stdin.
fn fake_bin(dir: &Path) -> (PathBuf, PathBuf) {
    let bin = dir.join("fake-quaqua");
    let log = dir.join("argv.log");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\ncat >/dev/null\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (bin, log)
}

/// Runs each matched command for `name` and returns the argv lines the fake saw.
fn argv_seen(settings: &Value, name: &str, bin: &Path, log: &Path) -> Vec<String> {
    let _ = std::fs::remove_file(log);
    let hook =
        json!({"tool_name": name, "tool_input": {"command": "echo hi", "file_path": "/tmp/x"}});
    for cmd in commands_for(settings, name) {
        let mut child = std::process::Command::new("sh")
            .args(["-c", &cmd])
            .env("QUAQUA_BIN", bin)
            .env("CLAUDE_PROJECT_DIR", template_root())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(hook.to_string().as_bytes())
            .unwrap();
        child.wait().unwrap();
    }
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn assert_routes(settings: &Value, bin: &Path, log: &Path, what: &str) {
    for name in ROUTED {
        let seen = argv_seen(settings, name, bin, log);
        assert!(
            seen.iter().any(|l| l.trim() == "gate"),
            "{what}: {name} must reach `quaqua gate`, saw {seen:?}"
        );
    }
    for name in UNROUTED {
        let seen = argv_seen(settings, name, bin, log);
        assert!(
            !seen.iter().any(|l| l.trim() == "gate"),
            "{what}: {name} must not reach `quaqua gate`, saw {seen:?}"
        );
    }
}

#[test]
fn hook_settings_route_template_reaches_gate() {
    let ws = TestWorkspace::new("hr-template");
    let (bin, log) = fake_bin(ws.root());
    let text = std::fs::read_to_string(template_root().join(".claude/settings.json")).unwrap();
    let settings: Value = serde_json::from_str(&text).unwrap();
    assert_routes(&settings, &bin, &log, "template settings");
}

#[test]
fn hook_settings_route_install_hook_reaches_gate() {
    let ws = TestWorkspace::new("hr-install");
    let wt = ws.root().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(&wt)
        .status()
        .unwrap()
        .success());
    let (bin, log) = fake_bin(ws.root());
    let reg = Registry::native();
    let h = reg
        .get("claude")
        .unwrap()
        .install_hook(&wt, &bin)
        .unwrap()
        .unwrap();
    let first = std::fs::read_to_string(&h.path).unwrap();
    let settings: Value = serde_json::from_str(&first).unwrap();
    assert_routes(&settings, &bin, &log, "install_hook");
    // Still one owned entry, and a repeat changes nothing.
    assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    reg.get("claude").unwrap().install_hook(&wt, &bin).unwrap();
    assert_eq!(first, std::fs::read_to_string(&h.path).unwrap());
}

// ---- the gate's MCP arm: a floor action ----------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Who {
    Cos,
    Worker,
    Down,
}

struct Rig {
    ws: TestWorkspace,
    herdr: Arc<FakeHerdr>,
    daemon: Option<DaemonHandle>,
}

fn rig(tag: &str, who: Who) -> Rig {
    let ws = TestWorkspace::new(tag);
    ws.seal_herdr();
    let herdr = Arc::new(FakeHerdr::new());
    if who == Who::Down {
        return Rig {
            ws,
            herdr,
            daemon: None,
        };
    }
    let mut d = qq_core::daemon::Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone() as Arc<dyn HerdrClient>,
    });
    d.require_peer(Arc::new(HerdrPeers::new(herdr.clone())));
    let daemon = d.spawn().unwrap();
    let me = std::process::id();
    if who == Who::Cos {
        let p = herdr.seed_pane("c", Some("claude"), &[("grid_role", "cos")]);
        herdr.set_pane_pids(&p, me, &[]);
    } else {
        let p = herdr.seed_pane("w", None, &[]);
        herdr.agent_start("w-1", "claude", &p, 0, &[]).unwrap();
        herdr.set_pane_pids(&p, me, &[]);
    }
    Rig {
        ws,
        herdr,
        daemon: Some(daemon),
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            self.herdr.clear(Verb::PaneList);
            let p = self.herdr.seed_pane("stop", None, &[]);
            self.herdr
                .agent_start("stopper", "claude", &p, 0, &[])
                .unwrap();
            self.herdr.set_pane_pids(&p, std::process::id(), &[]);
            let _ = d.stop();
        }
    }
}

impl Rig {
    fn tool(&self, hook: Value) -> GateVerdict {
        let env = Env {
            runs_dir: self.ws.runs_dir(),
        };
        gate_evaluate_in(&env, &hook.to_string())
    }
}

fn mcp(name: &str) -> Value {
    json!({"tool_name": name, "tool_input": {"query": "x"}})
}

#[test]
fn hook_settings_route_mcp_floor() {
    let spend = "mcp__plugin_tinyfish_tinyfish__top_up_wallet";
    let publish = "mcp__claude_ai_Claude_Data__share_connector";
    let send = "mcp__claude_ai_Slack__send_message";
    {
        let r = rig("hr-cos", Who::Cos);
        for n in [spend, publish, send] {
            assert!(
                matches!(r.tool(mcp(n)), GateVerdict::Ask(_)),
                "cos asks: {n}"
            );
        }
        // Every name the matchers route to the gate as outward is a floor hit.
        for n in ROUTED.iter().filter(|n| n.starts_with("mcp__")) {
            assert!(
                matches!(r.tool(mcp(n)), GateVerdict::Ask(_)),
                "cos asks: {n}"
            );
        }
        // A string `command` input must not turn the call into a plain Bash scan.
        let sneaky = json!({"tool_name": spend, "tool_input": {"command": "ls"}});
        assert!(matches!(r.tool(sneaky), GateVerdict::Ask(_)));
        // Everything else still passes: other MCP tools and a plain Write.
        assert_eq!(
            r.tool(mcp("mcp__claude_ai_Meetings__get_meeting")),
            GateVerdict::Allow
        );
        let w = json!({"tool_name": "Write", "tool_input": {"file_path": "/tmp/notes.md"}});
        assert_eq!(r.tool(w), GateVerdict::Allow);
    }
    for (who, tag) in [(Who::Worker, "hr-w"), (Who::Down, "hr-d")] {
        let r = rig(tag, who);
        for n in [spend, publish, send] {
            assert!(matches!(r.tool(mcp(n)), GateVerdict::Block(_)), "deny: {n}");
        }
    }
    // No workspace at all: deny, as for every approval-list hit.
    assert!(matches!(
        gate_evaluate(&mcp(spend).to_string()),
        GateVerdict::Block(_)
    ));
}

// ---- herdr-gate.sh fallback with the binary missing -----------------------

fn fallback_exit(hook: &Value) -> i32 {
    let script = template_root().join("scripts/hooks/herdr-gate.sh");
    let mut child = std::process::Command::new("bash")
        .arg(&script)
        .env("QUAQUA_BIN", "qq-binary-that-does-not-exist")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(hook.to_string().as_bytes())
        .unwrap();
    child.wait().unwrap().code().unwrap()
}

fn bash(cmd: &str) -> Value {
    json!({"tool_name": "Bash", "tool_input": {"command": cmd}})
}

#[test]
fn hook_settings_route_fallback_missing_binary() {
    // Outward MCP tools: exact and predictive.
    for n in ROUTED.iter().filter(|n| n.starts_with("mcp__")) {
        assert_eq!(fallback_exit(&mcp(n)), 2, "must deny: {n}");
    }
    for n in UNROUTED {
        assert_eq!(fallback_exit(&mcp(n)), 0, "must pass: {n}");
    }
    // The approval list T3 added to the binary.
    for cmd in [
        "git tag v1",
        "git tag -a v1 -m x",
        "git tag -d v1",
        "git branch -D feat",
        "git branch --delete feat",
        "git branch -m main old",
        "git branch -M master",
        "gh release create v1",
        "git filter-branch --all",
        "git update-ref refs/heads/main abc",
    ] {
        assert_eq!(fallback_exit(&bash(cmd)), 2, "must deny: {cmd}");
    }
    for cmd in [
        "git status",
        "git tag",
        "git tag -l",
        "git tag --list 'v*'",
        "git branch --list",
        "git branch -a",
        "gh release list",
        "cargo test",
    ] {
        assert_eq!(fallback_exit(&bash(cmd)), 0, "must pass: {cmd}");
    }
    // Write/Edit: only the live instruction files, and the content is not scanned.
    let w = |tool: &str, path: &str, body: &str| json!({"tool_name": tool, "tool_input": {"file_path": path, "content": body}});
    assert_eq!(
        fallback_exit(&w(
            "Write",
            "/tmp/notes.md",
            "run git push and herdr pane split"
        )),
        0
    );
    assert_eq!(fallback_exit(&w("Edit", "/tmp/a.rs", "git tag v1")), 0);
    for f in ["AGENTS.md", "identity.md"] {
        for t in ["Write", "Edit", "MultiEdit"] {
            assert_eq!(fallback_exit(&w(t, &format!("/ws/{f}"), "x")), 2, "{t} {f}");
        }
    }
}
