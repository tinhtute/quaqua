use qq_core::herdr::liveness::{worker_liveness_state, Liveness, WorkerState};
use qq_core::herdr::{CliHerdr, FakeHerdr, HerdrClient, Reply, Verb, NAME_CLASH, PANE_BUSY};
use qq_core::testkit::TestWorkspace;
use std::io::ErrorKind;

fn started(f: &FakeHerdr, name: &str, pane: &str) {
    assert!(f.agent_start(name, "claude", pane, 30_000, &[]).unwrap().ok);
}

#[test]
fn herdr_fake_contract() {
    let f = FakeHerdr::new();

    // start -> get: the agent is listed and reads back idle.
    started(&f, "w", "%1");
    let got = f.agent_get("w").unwrap();
    assert!(got.ok && got.body.contains(r#""agent_status":"idle""#));
    // start twice on the same pane converges, no second agent.
    started(&f, "w", "%1");
    assert_eq!(f.agent_list().unwrap().body.matches(r#""name""#).count(), 1);

    // busy pane: a different agent cannot take an occupied pane.
    let busy = f.agent_start("x", "claude", "%1", 30_000, &[]).unwrap();
    assert!(!busy.ok);
    assert_eq!(busy.error_code().as_deref(), Some(PANE_BUSY));
    assert_eq!(
        f.agent_get("x").unwrap().error_code().as_deref(),
        Some("agent_not_found")
    );

    // prompt delivers the text and flips the agent to working; wait sees idle after set_status.
    let r = f.agent_prompt("w", "do it", None).unwrap();
    assert!(r.ok);
    assert_eq!(f.prompts(), vec![("w".to_string(), "do it".to_string())]);
    assert_eq!(
        f.agent_wait("w", &["idle"], 10)
            .unwrap()
            .error_code()
            .as_deref(),
        Some("timeout")
    );
    f.set_status("w", "idle");
    assert!(f.agent_wait("w", &["idle"], 10).unwrap().ok);
    // default wait set is idle/done/blocked, not working.
    f.set_status("w", "working");
    assert!(!f.agent_wait("w", &[], 10).unwrap().ok);
    f.set_status("w", "blocked");
    assert!(f.agent_wait("w", &[], 10).unwrap().ok);

    // blocked agent: prompt rejected, nothing sent.
    let rej = f.agent_prompt("w", "again", None).unwrap();
    assert_eq!(rej.error_code().as_deref(), Some("agent_blocked"));
    assert_eq!(f.prompts().len(), 1);

    // prompt --wait: delivered, and the wait sees the working state the prompt started.
    f.set_status("w", "idle");
    assert!(
        f.agent_prompt("w", "next", Some((&["working"], 10)))
            .unwrap()
            .ok
    );
    assert_eq!(f.prompts().len(), 2);
}

#[test]
fn liveness_needs_two_checks_to_call_down() {
    let f = FakeHerdr::new();
    started(&f, "other", "%9");
    // gone: get says not found and list parses, non-empty, without the name.
    assert_eq!(qq_core::herdr::liveness::liveness(&f, "w"), Liveness::Down);
    // empty list is not proof.
    f.remove("other");
    assert!(matches!(
        qq_core::herdr::liveness::liveness(&f, "w"),
        Liveness::Unknown(_)
    ));
    // transport failure is unknown, never down.
    started(&f, "other", "%9");
    f.fail_next(ErrorKind::TimedOut);
    assert!(matches!(
        qq_core::herdr::liveness::liveness(&f, "w"),
        Liveness::Unknown(_)
    ));
    // alive.
    started(&f, "w", "%1");
    assert!(matches!(
        qq_core::herdr::liveness::liveness(&f, "w"),
        Liveness::Alive(_)
    ));
}

#[test]
fn liveness_state_combines_hook_file_and_screen() {
    let ws = TestWorkspace::new("liveness");
    let f = FakeHerdr::new();
    started(&f, "w1", "%1");
    let now = 1_730_000_500;
    let st = |f: &FakeHerdr| worker_liveness_state(f, &ws.runs_dir(), "w1", now, 600);

    assert_eq!(st(&f), WorkerState::Idle);
    let file = ws.runs_dir().join(".w1.state");
    std::fs::write(&file, format!("working {}\n", now - 10)).unwrap();
    assert_eq!(st(&f), WorkerState::Working);
    std::fs::write(&file, format!("working {}\n", now - 1000)).unwrap();
    assert_eq!(st(&f), WorkerState::Idle);
    f.remove("w1");
    started(&f, "keep", "%2");
    assert_eq!(st(&f), WorkerState::Dead);
}

#[test]
fn testkit_workspace_points_team_toml_at_temp_and_restores() {
    let before = std::env::var_os("QUAQUA_TEAM_TOML");
    {
        let ws = TestWorkspace::new("kit");
        assert_eq!(
            std::env::var_os("QUAQUA_TEAM_TOML"),
            Some(ws.team_toml().into_os_string())
        );
        assert!(ws.db_path().starts_with(ws.root()));
        assert!(!ws.db_path().exists());
    }
    assert_eq!(std::env::var_os("QUAQUA_TEAM_TOML"), before);
}

#[test]
fn hook_state_beats_screen_only_when_fresh() {
    use qq_core::herdr::liveness::{status_from, HookState};
    let working = HookState::parse("working 950").unwrap();
    assert_eq!(
        status_from(Some(&working), "idle", 1000, 600),
        WorkerState::Working
    );
    assert_eq!(
        status_from(Some(&working), "idle", 2000, 600),
        WorkerState::Idle
    );
    assert_eq!(
        status_from(Some(&working), "dead", 1000, 600),
        WorkerState::Dead
    );
    assert_eq!(
        status_from(None, "blocked", 1000, 600),
        WorkerState::Blocked
    );
}

#[cfg(unix)]
#[test]
fn cli_start_appends_agent_args_after_timeout() {
    use std::os::unix::fs::PermissionsExt;
    let ws = TestWorkspace::new("cli-args");
    let out = ws.root().join("argv.txt");
    let bin = ws.root().join("herdr-stub.sh");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho '{{}}'\n", out.display()),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cli = CliHerdr {
        bin: bin.to_string_lossy().into_owned(),
    };
    let r = cli
        .agent_start("w", "omp", "%1", 5000, &["--model", "m1", "--thinking", "high"])
        .unwrap();
    assert!(r.ok);
    let argv: Vec<String> = std::fs::read_to_string(&out)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        argv,
        [
            "agent", "start", "w", "--kind", "omp", "--pane", "%1", "--timeout", "5000", "--",
            "--model", "m1", "--thinking", "high"
        ]
    );
    // no extra args: no bare `--`.
    cli.agent_start("w", "omp", "%1", 5000, &[]).unwrap();
    let argv = std::fs::read_to_string(&out).unwrap();
    assert!(!argv.lines().any(|l| l == "--"));
}

#[test]
fn fake_start_remembers_agent_args() {
    let f = FakeHerdr::new();
    f.agent_start("w", "omp", "%1", 1, &["--model", "m1"]).unwrap();
    assert_eq!(f.start_args("w"), ["--model", "m1"]);
}

#[test]
fn fake_injects_reply_or_fault_per_verb() {
    use qq_core::herdr::liveness::liveness;
    let f = FakeHerdr::new();
    started(&f, "other", "%9");
    // get says not found (real), list is garbage: only that verb is faulted.
    f.set_reply(Verb::List, Reply::ok("garbage"));
    assert!(matches!(liveness(&f, "w"), Liveness::Unknown(_)));
    assert!(f.agent_get("other").unwrap().ok);
    // list times out; get still answers.
    f.set_fault(Verb::List, ErrorKind::TimedOut);
    assert_eq!(f.agent_list().unwrap_err().kind(), ErrorKind::TimedOut);
    assert!(f.agent_get("other").unwrap().ok);
    // sticky until cleared, then real behaviour is back.
    assert!(f.agent_list().is_err());
    f.clear(Verb::List);
    assert_eq!(liveness(&f, "w"), Liveness::Down);
    // an injected get reply beats the real agent.
    f.set_reply(Verb::Get, Reply::err("agent_not_found"));
    assert_eq!(f.agent_get("other").unwrap().error_code().as_deref(), Some("agent_not_found"));
}

#[test]
fn prompt_then_wait_for_done_succeeds_with_scripted_status() {
    let f = FakeHerdr::new();
    started(&f, "w", "%1");
    f.script_status("w", &["working", "done"]);
    let r = f
        .agent_prompt("w", "go", Some((&["idle", "done"], 1000)))
        .unwrap();
    assert!(r.ok, "{}", r.body);
    assert!(r.body.contains(r#""agent_status":"done""#));
    // no script: the wait still times out (the fake has no clock).
    started(&f, "v", "%2");
    let r = f.agent_prompt("v", "go", Some((&["done"], 10))).unwrap();
    assert_eq!(r.error_code().as_deref(), Some("timeout"));
}

#[test]
fn fake_is_shareable_across_threads() {
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<FakeHerdr>();
    assert_send_sync::<CliHerdr>();
    assert_send_sync::<dyn HerdrClient>();

    let f = std::sync::Arc::new(FakeHerdr::new());
    started(&f, "w", "%1");
    f.script_status("w", &["done"]);
    let worker = {
        let f = f.clone();
        std::thread::spawn(move || f.agent_prompt("w", "go", Some((&["done"], 1000))).unwrap())
    };
    assert!(worker.join().unwrap().ok);
    assert_eq!(f.prompts().len(), 1);
}

#[test]
fn name_clash_is_not_a_busy_pane() {
    let f = FakeHerdr::new();
    started(&f, "w", "%1");
    assert_ne!(PANE_BUSY, NAME_CLASH);
    // same name, other pane / other kind: name clash.
    let r = f.agent_start("w", "claude", "%2", 1, &[]).unwrap();
    assert_eq!(r.error_code().as_deref(), Some(NAME_CLASH));
    let r = f.agent_start("w", "codex", "%1", 1, &[]).unwrap();
    assert_eq!(r.error_code().as_deref(), Some(NAME_CLASH));
    // other name on the occupied pane: busy pane.
    let r = f.agent_start("x", "claude", "%1", 1, &[]).unwrap();
    assert_eq!(r.error_code().as_deref(), Some(PANE_BUSY));
}
