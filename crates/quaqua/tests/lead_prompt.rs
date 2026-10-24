//! `quaqua lead prompt <slug> <msg>` against a stub herdr (issue #55 item 1):
//! the input is cleared, submitted with wait, and the command exits non-zero
//! when the prompt did not land (draft left in the box / submit rejected).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    root: PathBuf,
    herdr: PathBuf,
    calls: PathBuf,
    state: PathBuf,
}
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}


impl Fixture {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("quaqua-lead-prompt-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let state = root.join("state");
        fs::create_dir_all(&state).unwrap();
        let herdr = root.join("herdr");
        fs::write(&herdr, SCRIPT).unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
        let calls = root.join("calls");
        fs::write(&calls, "").unwrap();
        Fixture {
            root,
            herdr,
            calls,
            state,
        }
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
        cmd.args(["lead", "prompt", "baro", "please fix the gate"])
            .env("HERDR_BIN_PATH", &self.herdr)
            .env("QUAQUA_TEST_STATE", &self.state)
            .env("QUAQUA_TEST_CALLS", &self.calls)
            .env("QUAQUA_LEAD_PROMPT_WAIT_MS", "300")
            .env_remove("HERDR_PANE_ID");
        for (k, _) in std::env::vars() {
            if k.starts_with("HERDR_") && k != "HERDR_BIN_PATH" {
                cmd.env_remove(&k);
            }
        }
        cmd
    }

    fn run(&self) -> Output {
        self.cmd().output().expect("run quaqua lead prompt")
    }

    fn calls(&self) -> String {
        fs::read_to_string(&self.calls).unwrap_or_default()
    }

    fn state_file(&self, name: &str) -> PathBuf {
        self.state.join(name)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const SCRIPT: &str = r#"#!/bin/bash
printf '%s\n' "$*" >> "$QUAQUA_TEST_CALLS"
if [[ "$1 $2" == "agent get" ]]; then
  if [[ -f "$QUAQUA_TEST_STATE/missing" ]]; then
    printf '%s\n' '{"error":{"code":"agent_not_found"}}'
    exit 1
  fi
  if [[ -f "$QUAQUA_TEST_STATE/stuck" ]]; then status=idle; else status=working; fi
  printf '{"result":{"agent":{"agent_status":"%s","pane_id":"%%p"}}}\n' "$status"
  exit 0
fi
if [[ "$1 $2" == "agent list" ]]; then
  if [[ -f "$QUAQUA_TEST_STATE/listed" ]]; then
    printf '%s\n' '{"result":{"agents":[{"name":"cos"},{"name":"baro-lead"}]}}'
  else
    printf '%s\n' '{"result":{"agents":[{"name":"cos"}]}}'
  fi
  exit 0
fi
if [[ "$1 $2" == "agent send-keys" ]]; then
  if [[ "$3" == "ctrl-u" || "$4" == "ctrl-u" ]]; then
    printf '%s\n' '{"code":"invalid_key","message":"unsupported key ctrl-u"}' >&2
    exit 1
  fi
  exit 0
fi
if [[ "$1 $2" == "agent prompt" ]]; then
  printf '%s' "$4" > "$QUAQUA_TEST_STATE/msg"
  if [[ -f "$QUAQUA_TEST_STATE/reject" ]]; then
    printf '%s\n' 'agent_prompt_stalled' >&2
    exit 1
  fi
  exit 0
fi
if [[ "$1 $2" == "pane read" ]]; then
  if [[ -f "$QUAQUA_TEST_STATE/stuck" ]]; then
    printf 'thinking\n  ┃ please fix the gate\n  Build auto\n  esc interrupt\n'
  else
    printf 'thinking\n  ┃\n  Build auto\n  esc interrupt\n'
  fi
  exit 0
fi
exit 0
"#;

#[test]
fn a_landed_prompt_clears_then_submits_with_wait() {
    let f = Fixture::new("landed");
    let out = f.run();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("prompted baro-lead"), "{stdout}");
    let calls = f.calls();
    assert_eq!(calls.lines().next(), Some("agent get baro-lead"));
    assert!(
        calls.contains("agent send-keys baro-lead ctrl+u"),
        "{calls}"
    );
    assert!(
        calls.contains("agent prompt baro-lead please fix the gate --wait --until working"),
        "{calls}"
    );
}
#[test]
fn landed_prompt_is_hermetic_when_ambient_herdr_pane_env_is_set() {
    let _lock = env_lock();
    let f = Fixture::new("ambient");
    let orig = std::env::var("HERDR_PANE_ID").ok();
    std::env::set_var("HERDR_PANE_ID", "w99:pAmbient");

    let out = f.run();

    if let Some(val) = &orig {
        std::env::set_var("HERDR_PANE_ID", val);
    } else {
        std::env::remove_var("HERDR_PANE_ID");
    }

    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let calls = f.calls();
    assert_eq!(
        calls.lines().next(),
        Some("agent get baro-lead"),
        "ambient HERDR_PANE_ID must not cause shadow to query pane before checking lead liveness: {calls}"
    );
}


#[test]
fn a_draft_left_in_the_input_box_is_nonzero() {
    let f = Fixture::new("stuck");
    fs::write(f.state_file("stuck"), "").unwrap();
    let out = f.run();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("did not land"), "{stderr}");
    assert!(stderr.contains("input box"), "{stderr}");
}

#[test]
fn a_rejected_submit_is_nonzero_and_names_the_reason() {
    let f = Fixture::new("rejected");
    fs::write(f.state_file("reject"), "").unwrap();
    let out = f.run();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("did not land"), "{stderr}");
    assert!(stderr.contains("agent_prompt_stalled"), "{stderr}");
}

#[test]
fn a_missing_lead_is_nonzero_with_the_spawn_hint() {
    let f = Fixture::new("missing");
    fs::write(f.state_file("missing"), "").unwrap();
    let out = f.run();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no live lead 'baro-lead'"), "{stderr}");
    assert!(stderr.contains("quaqua lead spawn"), "{stderr}");
}

#[test]
fn a_not_found_lead_still_listed_is_unknown_and_not_prompted() {
    let f = Fixture::new("listed");
    fs::write(f.state_file("missing"), "").unwrap();
    fs::write(f.state_file("listed"), "").unwrap();
    let out = f.run();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot determine"), "{stderr}");
    assert!(!stderr.contains("no live lead"), "{stderr}");
    assert!(!f.calls().contains("agent prompt"), "{}", f.calls());
}

#[allow(dead_code)]
fn _exists(p: &Path) -> bool {
    p.exists()
}
