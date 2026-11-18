use qq_core::authority::Fence;
use qq_core::config::{Driver, Harness};
use qq_core::driver::{start, stop, Registry, StartError};
use qq_core::herdr::{FakeHerdr, HerdrClient, Verb};
use qq_core::testkit::TestWorkspace;
use std::io::ErrorKind;
use std::path::Path;

fn harness(name: &str, driver: Driver) -> Harness {
    Harness {
        name: name.into(),
        driver,
        spec: None,
    }
}

#[test]
fn driver_registry_generic_from_config() {
    let reg = Registry::from_config(&[
        harness("claude", Driver::Native),
        harness("pi", Driver::Native),
        harness("kimi", Driver::Generic),
    ]);
    let g = reg.get("kimi").expect("generic driver from config");
    assert_eq!(g.herdr_kind(), "kimi");
    assert_eq!(g.fence(), Fence::Unfenced);
    assert!(g.launch_args("any", Some("high")).is_empty());
    assert!(g.stop_sequences().is_empty());

    // natives keep their own driver and fence; config cannot turn one generic.
    let reg = Registry::from_config(&[harness("pi", Driver::Generic)]);
    let pi = reg.get("pi").unwrap();
    assert_eq!(pi.fence(), Fence::Fenced);
    assert_eq!(pi.stop_sequences()[0], vec!["esc".to_string()]);
    assert!(Registry::native().get("kimi").is_none());
}

#[test]
fn no_fallback_on_pane_busy() {
    let reg = Registry::native();
    let f = FakeHerdr::new();
    let claude = reg.get("claude").unwrap();
    let pi = reg.get("pi").unwrap();
    start(&f, &*claude, "a", "%1", 1000, "opus", None).unwrap();

    // pane busy: reported, never retried or answered with another harness.
    let err = start(&f, &*pi, "b", "%1", 1000, "m", None).unwrap_err();
    assert_eq!(err, StartError::PaneBusy);
    let list = f.agent_list().unwrap().body;
    assert_eq!(list.matches(r#""name""#).count(), 1, "{list}");
    assert!(!list.contains(r#""pi""#), "{list}");

    // a transport blip is retried and converges; a persistent one is reported.
    f.fail_next(ErrorKind::TimedOut);
    assert!(start(&f, &*pi, "c", "%2", 1000, "m", None).is_ok());
    f.set_fault(Verb::Start, ErrorKind::TimedOut);
    assert!(matches!(
        start(&f, &*pi, "d", "%3", 1000, "m", None),
        Err(StartError::Transport(_))
    ));
}

fn git_wt(ws: &TestWorkspace) -> std::path::PathBuf {
    let wt = ws.root().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(&wt)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    wt
}

fn script(dir: &Path, name: &str, code: i32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\ncat >/dev/null\nexit {code}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn exclude(wt: &Path) -> String {
    std::fs::read_to_string(wt.join(".git/info/exclude")).unwrap_or_default()
}

fn claude_cmd(path: &Path) -> String {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_string()
}

fn run_cmd(cmd: &str) -> i32 {
    std::process::Command::new("sh")
        .args(["-c", cmd])
        .stdin(std::process::Stdio::null())
        .status()
        .unwrap()
        .code()
        .unwrap()
}

#[test]
fn native_hook_installed_per_driver() {
    let ws = TestWorkspace::new("driver-hooks");
    let wt = git_wt(&ws);
    std::fs::create_dir_all(wt.join(".claude")).unwrap();
    std::fs::write(
        wt.join(".claude/settings.local.json"),
        r#"{"permissions":{"deny":["Agent"]}}"#,
    )
    .unwrap();
    std::fs::write(wt.join(".claude/settings.json"), "tracked").unwrap();
    let gate = Path::new("/gate/quaqua");
    let reg = Registry::from_config(&[harness("kimi", Driver::Generic)]);

    // claude: local settings only, existing kept, idempotent, tracked file untouched.
    let claude = reg.get("claude").unwrap();
    let h = claude.install_hook(&wt, gate).unwrap().unwrap();
    assert_eq!(h.path, wt.join(".claude/settings.local.json"));
    let first = std::fs::read_to_string(&h.path).unwrap();
    claude.install_hook(&wt, gate).unwrap();
    assert_eq!(first, std::fs::read_to_string(&h.path).unwrap());
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["permissions"]["deny"][0], "Agent");
    assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert!(claude_cmd(&h.path).starts_with("'/gate/quaqua' gate"));
    assert_eq!(
        std::fs::read_to_string(wt.join(".claude/settings.json")).unwrap(),
        "tracked"
    );

    // pi: extension written once, loaded by launch args, blocks via tool_call.
    let pi = reg.get("pi").unwrap();
    let h = pi.install_hook(&wt, gate).unwrap().unwrap();
    let src = std::fs::read_to_string(&h.path).unwrap();
    assert!(src.contains("tool_call") && src.contains("block: true"));
    assert!(src.contains("/gate/quaqua"));
    pi.install_hook(&wt, gate).unwrap();
    assert_eq!(src, std::fs::read_to_string(&h.path).unwrap());
    let args = pi.launch_args("m", None);
    assert!(args
        .windows(2)
        .any(|w| w[0] == "-e" && wt.join(&w[1]) == h.path));
    assert_eq!(pi.fence(), Fence::Fenced);
    assert_eq!(claude.fence(), Fence::Fenced);

    // both hook files are excluded from git, once each.
    let ex = exclude(&wt);
    for pat in ["/.claude/settings.local.json", "/.qq/pi-gate.ts"] {
        assert_eq!(ex.lines().filter(|l| *l == pat).count(), 1, "{ex}");
    }

    // generic: no hook, written nothing, unfenced.
    let g = reg.get("kimi").unwrap();
    assert!(g.install_hook(&wt, gate).unwrap().is_none());
    assert_eq!(g.fence(), Fence::Unfenced);
}

#[test]
fn hook_fails_closed_on_gate_exit() {
    let ws = TestWorkspace::new("driver-failclosed");
    let wt = git_wt(&ws);
    let bin = ws.root().join("a dir");
    std::fs::create_dir_all(&bin).unwrap();
    let reg = Registry::native();

    // claude: any non-zero gate exit (1, 2, 127, missing) becomes exit 2.
    let claude = reg.get("claude").unwrap();
    for (code, want) in [(0, 0), (1, 2), (2, 2), (127, 2)] {
        let gate = script(&bin, &format!("g{code}"), code);
        let h = claude.install_hook(&wt, &gate).unwrap().unwrap();
        assert_eq!(run_cmd(&claude_cmd(&h.path)), want, "gate exit {code}");
    }
    let h = claude
        .install_hook(&wt, &bin.join("missing"))
        .unwrap()
        .unwrap();
    assert_eq!(run_cmd(&claude_cmd(&h.path)), 2);

    // pi: allows only on a spawned gate exiting 0; no regex fallback.
    let pi = reg.get("pi").unwrap();
    let h = pi.install_hook(&wt, &bin.join("g0")).unwrap().unwrap();
    let src = std::fs::read_to_string(&h.path).unwrap();
    assert!(src.contains("!r.error && r.status === 0"), "{src}");
    assert!(!src.contains("PUSH") && !src.contains("push"), "{src}");
    assert!(src.contains(r#"["gate"]"#));
}

#[test]
fn claude_hook_replaces_by_marker_and_keeps_bad_settings() {
    let ws = TestWorkspace::new("driver-marker");
    let wt = git_wt(&ws);
    let claude = Registry::native().get("claude").unwrap();
    let path = wt.join(".claude/settings.local.json");
    std::fs::create_dir_all(wt.join(".claude")).unwrap();
    std::fs::write(
        &path,
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/mine"}]}]}}"#,
    )
    .unwrap();

    // a new gate path replaces our entry, never appends; the user's stays.
    claude.install_hook(&wt, Path::new("/old/quaqua")).unwrap();
    claude.install_hook(&wt, Path::new("/new/quaqua")).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let list = v["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{v}");
    assert_eq!(list[0]["hooks"][0]["command"], "/mine");
    let cmd = list[1]["hooks"][0]["command"].as_str().unwrap();
    assert!(
        cmd.contains("/new/quaqua") && !cmd.contains("/old/"),
        "{cmd}"
    );

    // an invalid settings file is left untouched and install fails.
    for bad in ["{not json", "[1]", r#"{"hooks":{"PreToolUse":{}}}"#] {
        std::fs::write(&path, bad).unwrap();
        assert!(claude.install_hook(&wt, Path::new("/g")).is_err(), "{bad}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bad);
    }
}

#[test]
fn pi_usage_assistant_only_and_unknown_cost() {
    let ws = TestWorkspace::new("driver-usage");
    let pi = Registry::native().get("pi").unwrap();
    let claude = Registry::native().get("claude").unwrap();
    let jsonl = ws.root().join("s.jsonl");
    let usage = |input: u64, cost: &str, role: &str| {
        format!(
            r#"{{"type":"message","message":{{"role":"{role}","usage":{{"input":{input},"output":10,"cacheRead":5,"cacheWrite":1{cost}}}}}}}"#
        )
    };
    let cost = |c: f64| format!(r#","cost":{{"total":{c}}}"#);
    let write = |lines: &[String]| {
        std::fs::write(&jsonl, lines.join("\n") + "\n{\"message\":{\"usa").unwrap()
    };

    // only assistant entries count; user entries and a half line are skipped.
    write(&[
        r#"{"type":"session"}"#.into(),
        usage(999, "", "user"),
        usage(100, &cost(0.25), "assistant"),
        usage(50, &cost(0.5), "assistant"),
    ]);
    let u = pi.usage(&jsonl).unwrap().unwrap();
    assert_eq!(
        (u.input, u.output, u.cache_read, u.cache_write),
        (150, 20, 10, 2)
    );
    assert!((u.cost_usd.unwrap() - 0.75).abs() < 1e-9);

    // a missing cost makes the cost unknown, not $0; tokens still count.
    write(&[
        usage(100, &cost(0.25), "assistant"),
        usage(50, "", "assistant"),
    ]);
    let u = pi.usage(&jsonl).unwrap().unwrap();
    assert_eq!(u.input, 150);
    assert_eq!(u.cost_usd, None);

    // no assistant entry: nothing known.
    write(&[usage(1, &cost(1.0), "user")]);
    assert!(pi.usage(&jsonl).unwrap().is_none());
    assert!(claude.usage(&jsonl).unwrap().is_none());
}

#[test]
fn stop_sequence_sent_in_order() {
    let reg = Registry::native();
    let pi = reg.get("pi").unwrap();
    let f = FakeHerdr::new();
    start(&f, &*pi, "w", "%1", 1000, "m", None).unwrap();
    f.set_status("w", "working");
    // only the third sequence (esc esc) idles this agent.
    f.idle_on_keys("w", &["esc", "esc"]);
    assert!(stop(&f, &*pi, "w", 10).unwrap());
    let sent: Vec<Vec<String>> = f.keys().into_iter().map(|(_, k)| k).collect();
    let want: Vec<Vec<&str>> = vec![vec!["esc"], vec!["ctrl+c"], vec!["esc", "esc"]];
    assert_eq!(sent, want);

    // already idle: converges, nothing sent.
    assert!(stop(&f, &*pi, "w", 10).unwrap());
    assert_eq!(f.keys().len(), 3);

    // nothing works: every sequence tried once, then false.
    f.set_status("w", "working");
    f.idle_on_keys("w", &["never"]);
    assert!(!stop(&f, &*pi, "w", 10).unwrap());
    assert_eq!(f.keys().len(), 3 + 4);

    // generic has no sequences: no keys, false.
    let g = Registry::from_config(&[harness("kimi", Driver::Generic)])
        .get("kimi")
        .unwrap();
    start(&f, &*g, "k", "%2", 1000, "m", None).unwrap();
    f.set_status("k", "working");
    assert!(!stop(&f, &*g, "k", 10).unwrap());
    assert_eq!(f.keys().len(), 7);
}

#[test]
fn stop_survives_per_sequence_errors() {
    let pi = Registry::native().get("pi").unwrap();
    let f = FakeHerdr::new();
    start(&f, &*pi, "w", "%1", 1000, "m", None).unwrap();
    f.set_status("w", "working");

    // send-keys failing is not an error: every sequence is tried, then false.
    f.set_fault(Verb::SendKeys, ErrorKind::TimedOut);
    assert!(!stop(&f, &*pi, "w", 10).unwrap());

    // a failing wait moves on to the next sequence, not out of the chain.
    f.clear(Verb::SendKeys);
    f.set_fault(Verb::Wait, ErrorKind::TimedOut);
    assert!(!stop(&f, &*pi, "w", 10).unwrap());
    assert_eq!(f.keys().len(), 4);
}
