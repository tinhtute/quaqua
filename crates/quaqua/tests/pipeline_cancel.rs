//! Integration tests for pipeline cancellation (`quaqua pipeline --cancel <task> [--reason <text>]`).
//!
//! Asserts user-observable contracts:
//! 1. Terminal events (`workflow_cancelled`) written to parent and non-terminal children.
//! 2. Idempotent: repeating cancel causes zero additional writes and converges cleanly.
//! 3. Refuses cancellation if any child worker pane is alive in herdr.
//! 4. `quaqua status --all` renders cancelled pipelines with phase `cancelled` and last event `workflow_cancelled`.

use quaqua::engine::journal::{EventRecord, Journal, SnapshotRecord};
use quaqua::engine::journal_sqlite::SqliteJournal;
use quaqua::git::Git;
use quaqua::herdr::{Herdr, HerdrOps};
use quaqua::pipeline::cancel::cancel_pipeline;
use quaqua::pipeline::handle_pipeline;
use quaqua::status::render_status_all;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

struct MockHerdr {
    agent_list_json: String,
}

impl MockHerdr {
    fn new(json: &str) -> Self {
        Self {
            agent_list_json: json.to_string(),
        }
    }

    fn empty() -> Self {
        Self::new(r#"{"result":{"agents":[]}}"#)
    }

    fn with_live_worker(name: &str, status: &str) -> Self {
        Self::new(&format!(
            r#"{{"result":{{"agents":[{{"name":"{name}","agent_status":"{status}","pane_id":"%1"}}]}}}}"#
        ))
    }
}

impl HerdrOps for MockHerdr {
    fn run(&self, _args: &[&str]) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn agent_list(&self) -> std::io::Result<String> {
        Ok(self.agent_list_json.clone())
    }
    fn agent_get(&self, _name: &str) -> std::io::Result<(bool, String)> {
        unimplemented!()
    }
    fn agent_read(&self, _name: &str, _source: &str, _lines: u32) -> std::io::Result<String> {
        unimplemented!()
    }
    fn agent_start(
        &self,
        _name: &str,
        _kind: &str,
        _pane: &str,
        _timeout_ms: u32,
        _extra_args: &[&str],
    ) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn pane_split(
        &self,
        _cwd: &str,
        _owner: &str,
        _envs: &[(&str, &str)],
    ) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn pane_close(&self, _pane: &str) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn pane_report_metadata(&self, _pane: &str, _tokens: &[&str]) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn pane_wait_output(&self, _pane: &str, _pattern: &str, _timeout_ms: u32) -> bool {
        unimplemented!()
    }
    fn pane_read(&self, _pane: &str, _source: &str, _lines: u32) -> std::io::Result<String> {
        unimplemented!()
    }
    fn pane_send_keys(&self, _pane: &str, _key: &str) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn agent_send_keys(&self, _name: &str, _key: &str) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn agent_prompt_nowait(&self, _name: &str, _prompt: &str) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn agent_prompt_wait_background(
        &self,
        _name: &str,
        _prompt: &str,
        _timeout_ms: u32,
        _log_file: std::fs::File,
    ) -> std::io::Result<std::process::Child> {
        unimplemented!()
    }
}

struct TestRunsDir {
    path: PathBuf,
}

impl TestRunsDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("quaqua-test-cancel-{tag}-{nanos}"));
        fs::create_dir_all(&path).expect("create test runs dir");
        Self { path }
    }

    fn db_path(&self) -> PathBuf {
        self.path.join("quaqua.db")
    }
}

impl Drop for TestRunsDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn test_pipeline_cancel_writes_terminal_events_to_parent_and_non_terminal_children() {
    let runs = TestRunsDir::new("events");
    let mut journal = SqliteJournal::open(&runs.db_path()).expect("open test db");

    let task = "p8u3";
    let parent_wf = format!("pipeline:{task}");
    let child_non_terminal = format!("pipeline:{task}:sude-p8u3-plan");
    let child_already_completed = format!("pipeline:{task}:suhuynh-p8u3-review-r1");

    // Parent workflow in pause_for_approval
    journal
        .append_event(
            &parent_wf,
            &EventRecord::new(1, 1000, "child_started", r#"{"id":"sude-p8u3-plan"}"#),
        )
        .unwrap();
    journal
        .append_event(
            &parent_wf,
            &EventRecord::new(2, 2000, "pause_for_approval", r#"{"reason":"wait"}"#),
        )
        .unwrap();
    journal
        .append_snapshot(
            &parent_wf,
            &SnapshotRecord::new(1, r#"{"phase":"plan","round":0}"#),
        )
        .unwrap();

    // Non-terminal child stuck at activity_scheduled
    journal
        .append_event(
            &child_non_terminal,
            &EventRecord::new(1, 1100, "activity_scheduled", r#"{"id":"act1"}"#),
        )
        .unwrap();

    // Already completed child
    journal
        .append_event(
            &child_already_completed,
            &EventRecord::new(1, 1200, "workflow_completed", r#"{"exit":0}"#),
        )
        .unwrap();

    let herdr = MockHerdr::empty();
    let res = cancel_pipeline(task, Some("abandoned by user"), &runs.path, &herdr);
    assert!(res.is_ok(), "cancel_pipeline must succeed: {res:?}");

    // Verify parent has workflow_cancelled terminal event
    let parent_events = journal.load_events(&parent_wf, 1).unwrap();
    assert_eq!(parent_events.len(), 3);
    let last_ev = parent_events.last().unwrap();
    assert_eq!(last_ev.kind, "workflow_cancelled");
    assert!(last_ev.payload_json.contains("abandoned by user"));

    // Verify parent latest snapshot has phase: cancelled
    let latest_snap = journal.load_latest_snapshot(&parent_wf).unwrap().unwrap();
    assert!(
        latest_snap.state_json.contains(r#""phase":"cancelled""#),
        "snapshot state must have phase cancelled, got: {}",
        latest_snap.state_json
    );

    // Verify non-terminal child received workflow_cancelled
    let child_events = journal.load_events(&child_non_terminal, 1).unwrap();
    assert_eq!(child_events.len(), 2);
    assert_eq!(child_events.last().unwrap().kind, "workflow_cancelled");

    // Verify already completed child was untouched
    let term_events = journal.load_events(&child_already_completed, 1).unwrap();
    assert_eq!(term_events.len(), 1);
    assert_eq!(term_events[0].kind, "workflow_completed");
}

#[test]
fn test_pipeline_cancel_is_idempotent() {
    let runs = TestRunsDir::new("idempotent");
    let mut journal = SqliteJournal::open(&runs.db_path()).expect("open test db");

    let task = "p8u4";
    let parent_wf = format!("pipeline:{task}");
    let child_wf = format!("pipeline:{task}:sude-p8u4-plan");

    journal
        .append_event(
            &parent_wf,
            &EventRecord::new(1, 1000, "child_started", "{}"),
        )
        .unwrap();
    journal
        .append_event(
            &child_wf,
            &EventRecord::new(1, 1100, "activity_scheduled", "{}"),
        )
        .unwrap();

    let herdr = MockHerdr::empty();

    // First cancel run
    let res1 = cancel_pipeline(task, Some("first cancel"), &runs.path, &herdr);
    assert!(res1.is_ok());

    let count_parent_1 = journal.load_events(&parent_wf, 1).unwrap().len();
    let count_child_1 = journal.load_events(&child_wf, 1).unwrap().len();
    assert_eq!(count_parent_1, 2);
    assert_eq!(count_child_1, 2);

    // Second cancel run
    let res2 = cancel_pipeline(task, Some("second cancel"), &runs.path, &herdr);
    assert!(res2.is_ok(), "second run must succeed cleanly");

    let count_parent_2 = journal.load_events(&parent_wf, 1).unwrap().len();
    let count_child_2 = journal.load_events(&child_wf, 1).unwrap().len();
    assert_eq!(
        count_parent_1, count_parent_2,
        "second run must not append new events to parent"
    );
    assert_eq!(
        count_child_1, count_child_2,
        "second run must not append new events to child"
    );
}

#[test]
fn test_pipeline_cancel_refuses_when_child_worker_pane_is_alive() {
    let runs = TestRunsDir::new("refuse-live");
    let mut journal = SqliteJournal::open(&runs.db_path()).expect("open test db");

    let task = "gatep4";
    let parent_wf = format!("pipeline:{task}");
    let child_wf = format!("pipeline:{task}:sude-gatep4-plan");

    journal
        .append_event(
            &parent_wf,
            &EventRecord::new(1, 1000, "child_started", "{}"),
        )
        .unwrap();
    journal
        .append_event(
            &child_wf,
            &EventRecord::new(1, 1100, "activity_scheduled", "{}"),
        )
        .unwrap();

    // Write a .pipeline file associating worker with pipeline
    fs::write(runs.path.join("sude-gatep4.pipeline"), format!("{task}\n")).unwrap();

    // Mock herdr with live sude-gatep4 worker
    let herdr = MockHerdr::with_live_worker("sude-gatep4", "working");

    let res = cancel_pipeline(task, None, &runs.path, &herdr);
    assert!(res.is_err(), "must refuse when child worker pane is alive");
    let (code, msg) = res.unwrap_err();
    assert_eq!(code, 1);
    assert!(
        msg.contains("child worker 'sude-gatep4' is still alive"),
        "error must cite the live worker, got: {msg}"
    );

    // Verify journal was completely unmutated
    let parent_events = journal.load_events(&parent_wf, 1).unwrap();
    assert_eq!(parent_events.len(), 1);
    assert_eq!(parent_events[0].kind, "child_started");

    let child_events = journal.load_events(&child_wf, 1).unwrap();
    assert_eq!(child_events.len(), 1);
    assert_eq!(child_events[0].kind, "activity_scheduled");
}

#[test]
fn test_status_shows_cancelled_pipeline_as_terminal_phase() {
    let runs = TestRunsDir::new("status");
    let mut journal = SqliteJournal::open(&runs.db_path()).expect("open test db");

    let task = "leadroute";
    let parent_wf = format!("pipeline:{task}");
    let child_wf = format!("pipeline:{task}:sude-leadroute-plan");

    journal
        .append_event(
            &parent_wf,
            &EventRecord::new(1, 1000, "pause_for_approval", r#"{"reason":"wait"}"#),
        )
        .unwrap();
    journal
        .append_snapshot(&parent_wf, &SnapshotRecord::new(1, r#"{"phase":"plan"}"#))
        .unwrap();
    journal
        .append_event(
            &child_wf,
            &EventRecord::new(1, 1100, "activity_scheduled", "{}"),
        )
        .unwrap();

    let herdr = MockHerdr::empty();

    // Cancel the pipeline
    cancel_pipeline(task, Some("abandoned"), &runs.path, &herdr).unwrap();

    // Query status report
    let status_out = render_status_all(&runs.db_path(), &runs.path, &herdr).expect("render status");

    // Parent workflow must be displayed with LAST_EVENT: workflow_cancelled and PHASE: cancelled
    let mut found_parent = false;
    let mut found_child = false;
    for line in status_out.lines() {
        if line.starts_with(&format!("pipeline:{task} ")) {
            found_parent = true;
            assert!(
                line.contains("workflow_cancelled"),
                "expected workflow_cancelled in line: {line}"
            );
            assert!(
                line.contains("cancelled"),
                "expected phase cancelled in line: {line}"
            );
        } else if line.starts_with(&format!("pipeline:{task}:")) {
            found_child = true;
            assert!(
                line.contains("workflow_cancelled"),
                "child line must show workflow_cancelled: {line}"
            );
        }
    }

    assert!(
        found_parent,
        "status table must contain parent pipeline row"
    );
    assert!(found_child, "status table must contain child workflow row");
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, val: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, val);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(v) = &self.prev {
            std::env::set_var(self.key, v);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn make_fake_herdr(dir: &Path, agents_json: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script_path = dir.join("fake_herdr.sh");
    let script = format!(
        "#!/usr/bin/env bash\n\
         if [ \"$1\" = \"agent\" ] && [ \"$2\" = \"list\" ]; then\n\
           echo '{agents_json}'\n\
           exit 0\n\
         fi\n\
         echo '{{\"result\":{{}}}}'\n\
         exit 0\n"
    );
    fs::write(&script_path, script).unwrap();
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
    script_path
}

#[test]
fn test_handle_pipeline_cli_cancel_dispatches_cleanly() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let runs = TestRunsDir::new("cli");
    let mut journal = SqliteJournal::open(&runs.db_path()).expect("open test db");

    let task = "cli-pipe";
    let parent_wf = format!("pipeline:{task}");

    journal
        .append_event(
            &parent_wf,
            &EventRecord::new(1, 1000, "pause_for_approval", r#"{"reason":"wait"}"#),
        )
        .unwrap();

    let fake_bin = make_fake_herdr(&runs.path, r#"{"result":{"agents":[]}}"#);
    let herdr = Herdr {
        bin: fake_bin.to_string_lossy().to_string(),
    };
    let git = Git::new();
    let _guard_runs = EnvGuard::set("QUAQUA_RUNS_DIR", runs.path.to_str().unwrap());

    let args = vec![
        "--cancel".to_string(),
        task.to_string(),
        "--reason".to_string(),
        "cli cancel test".to_string(),
    ];

    let res = handle_pipeline(&args, &git, &herdr);
    assert!(
        res.is_ok(),
        "handle_pipeline --cancel must succeed: {res:?}"
    );

    let events = journal.load_events(&parent_wf, 1).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events.last().unwrap().kind, "workflow_cancelled");
    assert!(events
        .last()
        .unwrap()
        .payload_json
        .contains("cli cancel test"));
}

#[test]
fn test_handle_pipeline_cli_cancel_validation_errors() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let runs = TestRunsDir::new("cli-err");
    let _guard_runs = EnvGuard::set("QUAQUA_RUNS_DIR", runs.path.to_str().unwrap());
    let fake_bin = make_fake_herdr(&runs.path, r#"{"result":{"agents":[]}}"#);
    let herdr = Herdr {
        bin: fake_bin.to_string_lossy().to_string(),
    };
    let git = Git::new();

    // Missing task name after --cancel
    let res = handle_pipeline(&["--cancel".to_string()], &git, &herdr);
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().0, 1);

    // Invalid slug
    let res = handle_pipeline(
        &["--cancel".to_string(), "INVALID_SLUG!".to_string()],
        &git,
        &herdr,
    );
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().0, 1);

    // Non-existent pipeline
    let res = handle_pipeline(
        &["--cancel".to_string(), "nonexistent".to_string()],
        &git,
        &herdr,
    );
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().0, 1);
}

#[test]
fn test_resume_cancelled_pipeline_is_refused() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let runs = TestRunsDir::new("resume-cancelled");
    let _guard_runs = EnvGuard::set("QUAQUA_RUNS_DIR", runs.path.to_str().unwrap());
    let mut journal = SqliteJournal::open(&runs.db_path()).expect("open test db");

    let task = "res-can";
    let parent_wf = format!("pipeline:{task}");

    journal
        .append_event(
            &parent_wf,
            &EventRecord::new(1, 1000, "pause_for_approval", r#"{"reason":"wait"}"#),
        )
        .unwrap();

    // Create state file for this pipeline
    let state_file = runs.path.join(format!("pipeline-{task}.state"));
    let repo_dir = runs.path.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(
        &state_file,
        format!(
            "repo={}\nbrief=b.md\nbuilder=sude\nreviewer=suhuynh\nreview=auto\nrounds=2\nbase=main\nwt=w\nbase_sha=s\nhead_sha=h\nplanfile=p\ncommit=c\nphase=plan\nround=0\nprompted=0\nprompt_ts=0\nrounds_run=0\nreview_run=auto\nreview_changed=false\nlast_findings=0\nfinished=0\nstart_elapsed=0\nbuilder_spawned=0\nreviewer_spawned=0\nlead=cos\nlead_pane=\npipeline_pid=0\n",
            repo_dir.display()
        ),
    )
    .unwrap();

    let fake_bin = make_fake_herdr(&runs.path, r#"{"result":{"agents":[]}}"#);
    let herdr = Herdr {
        bin: fake_bin.to_string_lossy().to_string(),
    };
    let git = Git::new();

    // 1. Cancel the pipeline
    let cancel_res = handle_pipeline(&["--cancel".to_string(), task.to_string()], &git, &herdr);
    assert!(cancel_res.is_ok());

    // 2. Try to resume the cancelled pipeline
    let resume_res = handle_pipeline(
        &[
            "--resume".to_string(),
            task.to_string(),
            repo_dir.to_str().unwrap().to_string(),
        ],
        &git,
        &herdr,
    );
    assert!(
        resume_res.is_err(),
        "resuming a cancelled pipeline must be refused"
    );
    let (code, msg) = resume_res.unwrap_err();
    assert_eq!(code, 1);
    assert!(
        msg.contains("was cancelled"),
        "expected 'was cancelled' in error message: {msg}"
    );
}

#[test]
fn built_binary_status_help_and_short_help_print_status_help() {
    for flag in ["--help", "-h"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_quaqua"))
            .args(["status", flag])
            .output()
            .expect("run quaqua status help");
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(text, quaqua::status::status_help(), "{flag}");
    }
}
