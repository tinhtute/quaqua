use qq_core::authority::Fence;
use qq_core::config::{Driver, Harness};
use qq_core::driver::{start, Registry, StartError};
use qq_core::herdr::{FakeHerdr, HerdrClient, Verb};
use qq_core::testkit::TestWorkspace;
use std::io::ErrorKind;
use std::path::Path;

fn harness(name: &str, driver: Driver) -> Harness {
    Harness {
        name: name.into(),
        driver,
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

#[test]
fn native_hook_installed_per_driver() {
    let ws = TestWorkspace::new("driver-hooks");
    let wt = ws.root().join("wt");
    std::fs::create_dir_all(wt.join(".claude")).unwrap();
    std::fs::write(
        wt.join(".claude/settings.json"),
        r#"{"permissions":{"deny":["Agent"]}}"#,
    )
    .unwrap();
    let gate = Path::new("/gate/herdr-gate.sh");
    let reg = Registry::from_config(&[harness("kimi", Driver::Generic)]);

    // claude: PreToolUse Bash hook merged in, existing settings kept, idempotent.
    let claude = reg.get("claude").unwrap();
    let h = claude.install_hook(&wt, gate).unwrap().unwrap();
    let first = std::fs::read_to_string(&h.path).unwrap();
    claude.install_hook(&wt, gate).unwrap();
    assert_eq!(first, std::fs::read_to_string(&h.path).unwrap());
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["permissions"]["deny"][0], "Agent");
    assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert_eq!(
        v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "/gate/herdr-gate.sh"
    );

    // pi: extension written once, loaded by launch args, blocks via tool_call.
    let pi = reg.get("pi").unwrap();
    let h = pi.install_hook(&wt, gate).unwrap().unwrap();
    let src = std::fs::read_to_string(&h.path).unwrap();
    assert!(src.contains("tool_call") && src.contains("block: true"));
    assert!(src.contains("/gate/herdr-gate.sh"));
    pi.install_hook(&wt, gate).unwrap();
    assert_eq!(src, std::fs::read_to_string(&h.path).unwrap());
    let args = pi.launch_args("m", None);
    assert!(args
        .windows(2)
        .any(|w| w[0] == "-e" && wt.join(&w[1]) == h.path));
    assert_eq!(pi.fence(), Fence::Fenced);
    assert_eq!(claude.fence(), Fence::Fenced);

    // generic: no hook, written nothing, unfenced.
    let g = reg.get("kimi").unwrap();
    assert!(g.install_hook(&wt, gate).unwrap().is_none());
    assert_eq!(g.fence(), Fence::Unfenced);

    // pi usage comes from the session jsonl, not the screen.
    let jsonl = ws.root().join("s.jsonl");
    std::fs::write(
        &jsonl,
        concat!(
            r#"{"type":"session"}"#, "\n",
            r#"{"message":{"usage":{"input":100,"output":10,"cacheRead":5,"cacheWrite":1,"cost":{"total":0.25}}}}"#, "\n",
            r#"{"message":{"usage":{"input":50,"output":5,"cacheRead":0,"cacheWrite":0,"cost":{"total":0.5}}}}"#, "\n",
            r#"{"message":{"usa"#,
        ),
    )
    .unwrap();
    let u = pi.usage(&jsonl).unwrap().unwrap();
    assert_eq!(
        (u.input, u.output, u.cache_read, u.cache_write),
        (150, 15, 5, 1)
    );
    assert!((u.cost_usd - 0.75).abs() < 1e-9);
    assert!(claude.usage(&jsonl).unwrap().is_none());
}
