use qq_core::herdr::liveness::{worker_liveness_state, Liveness, WorkerState};
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::testkit::TestWorkspace;
use std::io::ErrorKind;

fn started(f: &FakeHerdr, name: &str, pane: &str) {
    assert!(f.agent_start(name, "claude", pane, 30_000).unwrap().ok);
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
    let busy = f.agent_start("x", "claude", "%1", 30_000).unwrap();
    assert!(!busy.ok);
    assert_eq!(busy.error_code().as_deref(), Some("pane_busy"));
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
