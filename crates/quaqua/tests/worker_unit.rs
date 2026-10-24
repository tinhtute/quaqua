use quaqua::cap::{count_owner_slots, is_task_worker_name, matches_pipeline_reviewer};
use quaqua::config;
use quaqua::engine::activity::{ActivityRequest, ActivityResult, HerdrRequest, HerdrResult};
use quaqua::engine::testkit::MockActivities;
use quaqua::git::GitOps;
use quaqua::herdr::HerdrOps;
use quaqua::launch::{build_launch_cmd, Launch};
use quaqua::lead::spawn::{update_index_content, update_project_index_owner};
use quaqua::worker::cleanup::{resolve_default_runs_dir, resolve_home_quaqua_runs};
use quaqua::worker::prompt::{brief_budget_sec, brief_headers_ok, validate_env_budget_val};
use quaqua::worker::spawn::{fallback_kind, parse_spawn_args, start_failure_reason};
use quaqua::workspace::resolve_workspace_root;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::Mutex;

#[test]
fn test_task_worker_name_validation() {
    let cfg = config::embedded_team_config().expect("embedded team.toml");
    assert!(is_task_worker_name("tieude-task1", &cfg));
    assert!(is_task_worker_name("sude-hardimpl", &cfg));
    assert!(is_task_worker_name("suhuynh-review2", &cfg));
    assert!(is_task_worker_name("ni-quickfix", &cfg));

    // Leads do not count as task workers
    assert!(!is_task_worker_name("coucou-lead", &cfg));
    assert!(!is_task_worker_name("sude-lead", &cfg));
    assert!(!is_task_worker_name("quaqua-rs-lead", &cfg));

    // Unknown kinds
    assert!(!is_task_worker_name("random-worker", &cfg));
    assert!(!is_task_worker_name("unknown-task", &cfg));
}

#[test]
fn test_brief_headers() {
    let valid =
        "Goal: accomplish task\nHard constraints: none\nCurrent design (open to challenge): plan";
    assert!(brief_headers_ok(valid));

    let valid_alt = "Goal: accomplish task\nHard constraints: none\nCurrent design: direct";
    assert!(brief_headers_ok(valid_alt));

    let invalid_missing_goal = "Hard constraints: none\nCurrent design: direct";
    assert!(!brief_headers_ok(invalid_missing_goal));

    let invalid_missing_design = "Goal: accomplish task\nHard constraints: none";
    assert!(!brief_headers_ok(invalid_missing_design));
}

#[test]
fn test_brief_budget_regex_matching() {
    // Happy paths
    assert_eq!(brief_budget_sec("Time budget: 25 min"), Some(1500));
    assert_eq!(brief_budget_sec("Time budget: 10m"), Some(600));
    assert_eq!(brief_budget_sec("Time budget: 45 minutes"), Some(2700));
    assert_eq!(brief_budget_sec("Time budget: 15 mins"), Some(900));
    assert_eq!(brief_budget_sec("time budget: 20 min."), Some(1200));

    // Finding 3 edge cases: must NOT match if unit is hours or seconds, even if line contains 'm'
    assert_eq!(brief_budget_sec("Time budget: 2 hours max"), None);
    assert_eq!(brief_budget_sec("Time budget: 1800s (team max)"), None);
    assert_eq!(brief_budget_sec("Time budget: 3 days"), None);
}

#[test]
fn test_brief_budget_non_ascii_n1() {
    // N1: multi-byte UTF-8 after 'Time budget:' must not panic and must match bash
    assert_eq!(brief_budget_sec("Time budget: ≈ 20 min"), Some(1200));
    assert_eq!(brief_budget_sec("Time budget: —"), None);
    assert_eq!(brief_budget_sec("Time budget: — 45 mins"), Some(2700));
    assert_eq!(brief_budget_sec("Time budget: ≈ 10m"), Some(600));
}

#[test]
fn test_env_budget_validation_n3_n5() {
    // N3: empty string = unset (valid, not err)
    // N5: no process-global set_var mutations
    assert!(validate_env_budget_val(Some("")).is_ok());
    assert!(validate_env_budget_val(None).is_ok());
    assert!(validate_env_budget_val(Some("120")).is_ok());

    // Invalid values
    assert!(validate_env_budget_val(Some("abc")).is_err());
    assert!(validate_env_budget_val(Some("12.5")).is_err());
    assert!(validate_env_budget_val(Some("-5")).is_err());
}

#[test]
fn test_pipeline_reviewer_prefix_matching_n2() {
    // N2: suhuynh-prep-pr1 has '-prep' before '-pr1'; must match on second occurrence
    assert!(matches_pipeline_reviewer("suhuynh-prep-pr1", "test"));
    assert!(matches_pipeline_reviewer("suhuynh-pr1", "test"));
    assert!(matches_pipeline_reviewer("suhuynh-pr24", "test"));
    assert!(matches_pipeline_reviewer("suhuynh-testr1", "test"));
    assert!(matches_pipeline_reviewer("suhuynh-testr3", "test"));

    // N2: empty task matches *-r[0-9]*
    assert!(matches_pipeline_reviewer("suhuynh-r3r2", ""));
    assert!(matches_pipeline_reviewer("ni-r1", ""));

    // Non-reviewer workers
    assert!(!matches_pipeline_reviewer("tieude-prompt", "test"));
    assert!(!matches_pipeline_reviewer("tieude-process", "test"));
    assert!(!matches_pipeline_reviewer("sude-testrun", "test"));
    assert!(!matches_pipeline_reviewer("sude-testreview", "test"));
}

/// Provisional adapter (W2 rewire) driving HerdrOps through MockActivities.
///
/// NOTE on W2 rewire: The current W0 testkit `HerdrRequest` enum does not have variants
/// for pane split, agent list, metadata reporting, or waiting. Consequently, this adapter
/// provisionally maps those operations onto available `HerdrRequest` variants (e.g. `Spawn`,
/// `AgentWait`, `Prompt`) and custom activity IDs (`"herdr.pane_split"`, `"herdr.agent_list"`,
/// etc.) as a temporary bridge until W2 updates the activity contract.
struct MockActivitiesHerdr {
    pub activities: Mutex<MockActivities>,
}

impl MockActivitiesHerdr {
    pub fn new(activities: MockActivities) -> Self {
        Self {
            activities: Mutex::new(activities),
        }
    }

    pub fn with_agent_list(json: &str) -> Self {
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.agent_list",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: json.to_string(),
            }),
        );
        Self::new(mock)
    }

    pub fn with_agent_get(json: &str) -> Self {
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.agent_get",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: json.to_string(),
            }),
        );
        Self::new(mock)
    }
    pub fn with_unreachable(io_message: &str) -> Self {
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.agent_get",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: format!("io_error:{io_message}"),
            }),
        );
        Self::new(mock)
    }

    pub fn for_cleanup(pane_id: &str) -> Self {
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.agent_get",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: format!(r#"{{"result":{{"agent":{{"pane_id":"{pane_id}"}}}}}}"#),
            }),
        );
        mock.register(
            "herdr.pane_close",
            ActivityResult::Herdr(HerdrResult::Cleaned {
                name: pane_id.to_string(),
            }),
        );
        Self::new(mock)
    }

    pub fn for_spawn() -> Self {
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.pane_split",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
            }),
        );
        mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
            name: String::new(),
        }));
        Self::new(mock)
    }

    pub fn calls(&self) -> Vec<(String, ActivityRequest)> {
        self.activities.lock().unwrap().calls().to_vec()
    }

    pub fn call_count(&self) -> usize {
        self.activities.lock().unwrap().call_count()
    }

    pub fn has_call(&self, activity_id: &str) -> bool {
        self.activities
            .lock()
            .unwrap()
            .calls()
            .iter()
            .any(|(id, _)| id == activity_id)
    }

    pub fn agent_starts(&self) -> Vec<(String, String)> {
        self.activities
            .lock()
            .unwrap()
            .calls()
            .iter()
            .filter(|(id, _)| id.starts_with("herdr.agent_start"))
            .filter_map(|(_, req)| {
                if let ActivityRequest::Herdr(HerdrRequest::Spawn { name, kind, .. }) = req {
                    Some((name.clone(), kind.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn send_keys_calls(&self) -> Vec<(String, String)> {
        self.activities
            .lock()
            .unwrap()
            .calls()
            .iter()
            .filter(|(id, _)| id == "herdr.agent_send_keys")
            .filter_map(|(_, req)| {
                if let ActivityRequest::Herdr(HerdrRequest::SendKeys { name, keys }) = req {
                    Some((name.clone(), keys.clone()))
                } else {
                    None
                }
            })
            .collect()
    }
}

impl HerdrOps for MockActivitiesHerdr {
    fn run(&self, args: &[&str]) -> std::io::Result<Output> {
        let req = ActivityRequest::Herdr(HerdrRequest::Prompt {
            name: args.join(" "),
            task: String::new(),
        });
        let mut acts = self.activities.lock().unwrap();
        match acts.execute("herdr.run", &req) {
            Ok(ActivityResult::Herdr(HerdrResult::AgentOutput { content })) => Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: content.into_bytes(),
                stderr: Vec::new(),
            }),
            Ok(_) => Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: b"{\"snapshot\":{\"workspaces\":[],\"tabs\":[],\"panes\":[],\"layouts\":[],\"agents\":[]}}".to_vec(),
                stderr: Vec::new(),
            }),
            Err(err) => Err(std::io::Error::new(std::io::ErrorKind::Other, err.to_string())),
        }
    }

    fn agent_list(&self) -> std::io::Result<String> {
        let req = ActivityRequest::Herdr(HerdrRequest::AgentWait { name: String::new(), until: Vec::new(), timeout_ms: 0 });
        let mut acts = self.activities.lock().unwrap();
        match acts.execute("herdr.agent_list", &req) {
            Ok(ActivityResult::Herdr(HerdrResult::AgentOutput { content })) => Ok(content),
            Ok(_) => Ok(r#"{"result":{"agents":[]}}"#.to_string()),
            Err(err) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            )),
        }
    }

    fn agent_get(&self, name: &str) -> std::io::Result<(bool, String)> {
        let req = ActivityRequest::Herdr(HerdrRequest::AgentWait { name: name.to_string(), until: Vec::new(), timeout_ms: 0 });
        let mut acts = self.activities.lock().unwrap();
        match acts.execute("herdr.agent_get", &req) {
            Ok(ActivityResult::Herdr(HerdrResult::AgentOutput { content })) => {
                if let Some(msg) = content.strip_prefix("io_error:") {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        msg.to_string(),
                    ))
                } else {
                    Ok((true, content))
                }
            }
            Ok(_) => Ok((false, String::new())),
            Err(err) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            )),
        }
    }

    fn agent_read(&self, name: &str, _source: &str, _lines: u32) -> std::io::Result<String> {
        let req = ActivityRequest::Herdr(HerdrRequest::AgentWait { name: name.to_string(), until: Vec::new(), timeout_ms: 0 });
        let mut acts = self.activities.lock().unwrap();
        match acts.execute("herdr.agent_read", &req) {
            Ok(ActivityResult::Herdr(HerdrResult::AgentOutput { content })) => Ok(content),
            Ok(_) => Ok(String::new()),
            Err(err) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            )),
        }
    }

    fn agent_start(
        &self,
        name: &str,
        kind: &str,
        pane: &str,
        _timeout_ms: u32,
        _extra_args: &[&str],
    ) -> std::io::Result<Output> {
        let req = ActivityRequest::Herdr(HerdrRequest::Spawn {
            name: name.to_string(),
            kind: kind.to_string(),
            repo: pane.to_string(),
            approve_kinds: Vec::new(),
        });
        let mut acts = self.activities.lock().unwrap();
        match acts.execute(&format!("herdr.agent_start:{kind}"), &req) {
            Ok(ActivityResult::Herdr(HerdrResult::AgentOutput { content })) => {
                if content.contains("\"error\"") || !content.is_empty() {
                    Ok(Output {
                        status: std::os::unix::process::ExitStatusExt::from_raw(1 << 8),
                        stdout: content.into_bytes(),
                        stderr: Vec::new(),
                    })
                } else {
                    Ok(Output {
                        status: std::os::unix::process::ExitStatusExt::from_raw(0),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    })
                }
            }
            Ok(_) => Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            Err(err) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            )),
        }
    }

    fn pane_split(
        &self,
        cwd: &str,
        owner: &str,
        _envs: &[(&str, &str)],
    ) -> std::io::Result<Output> {
        let req = ActivityRequest::Herdr(HerdrRequest::Spawn {
            name: owner.to_string(),
            kind: "split".to_string(),
            repo: cwd.to_string(),
            approve_kinds: Vec::new(),
        });
        let mut acts = self.activities.lock().unwrap();
        match acts.execute("herdr.pane_split", &req) {
            Ok(ActivityResult::Herdr(HerdrResult::AgentOutput { content })) => Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: content.into_bytes(),
                stderr: Vec::new(),
            }),
            Ok(_) => Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: b"{\"result\":{\"pane\":{\"pane_id\":\"%1\"}}}".to_vec(),
                stderr: Vec::new(),
            }),
            Err(err) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            )),
        }
    }

    fn pane_close(&self, pane: &str) -> std::io::Result<Output> {
        let req = ActivityRequest::Herdr(HerdrRequest::Cleanup {
            name: pane.to_string(),
            repo: String::new(),
        });
        let mut acts = self.activities.lock().unwrap();
        match acts.execute("herdr.pane_close", &req) {
            Ok(_) => Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            Err(err) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            )),
        }
    }

    fn pane_report_metadata(&self, pane: &str, tokens: &[&str]) -> std::io::Result<Output> {
        let req = ActivityRequest::Herdr(HerdrRequest::SendKeys {
            name: pane.to_string(),
            keys: tokens.join(" "),
        });
        let mut acts = self.activities.lock().unwrap();
        let _ = acts.execute("herdr.pane_report_metadata", &req);
        Ok(Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn pane_wait_output(&self, pane: &str, _pattern: &str, _timeout_ms: u32) -> bool {
        let req = ActivityRequest::Herdr(HerdrRequest::AgentWait { name: pane.to_string(), until: Vec::new(), timeout_ms: 0 });
        let mut acts = self.activities.lock().unwrap();
        let _ = acts.execute("herdr.pane_wait_output", &req);
        false
    }

    fn pane_read(&self, pane: &str, _source: &str, _lines: u32) -> std::io::Result<String> {
        let req = ActivityRequest::Herdr(HerdrRequest::AgentWait { name: pane.to_string(), until: Vec::new(), timeout_ms: 0 });
        let mut acts = self.activities.lock().unwrap();
        match acts.execute("herdr.pane_read", &req) {
            Ok(ActivityResult::Herdr(HerdrResult::AgentOutput { content })) => Ok(content),
            Ok(_) => Ok(String::new()),
            Err(err) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            )),
        }
    }

    fn pane_send_keys(&self, pane: &str, key: &str) -> std::io::Result<Output> {
        let req = ActivityRequest::Herdr(HerdrRequest::SendKeys {
            name: pane.to_string(),
            keys: key.to_string(),
        });
        let mut acts = self.activities.lock().unwrap();
        let _ = acts.execute("herdr.pane_send_keys", &req);
        Ok(Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn agent_send_keys(&self, name: &str, key: &str) -> std::io::Result<Output> {
        let req = ActivityRequest::Herdr(HerdrRequest::SendKeys {
            name: name.to_string(),
            keys: key.to_string(),
        });
        let mut acts = self.activities.lock().unwrap();
        let _ = acts.execute("herdr.agent_send_keys", &req);
        Ok(Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn agent_wait_idle(&self, name: &str, _timeout_ms: u32) -> std::io::Result<Output> {
        let req = ActivityRequest::Herdr(HerdrRequest::Prompt {
            name: name.to_string(),
            task: String::new(),
        });
        let mut acts = self.activities.lock().unwrap();
        match acts.execute("herdr.agent_wait_idle", &req) {
            Ok(ActivityResult::Herdr(HerdrResult::Prompted { .. })) => Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            _ => Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(1 << 8),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
        }
    }

    fn agent_prompt_nowait(&self, name: &str, prompt: &str) -> std::io::Result<Output> {
        let req = ActivityRequest::Herdr(HerdrRequest::Prompt {
            name: name.to_string(),
            task: prompt.to_string(),
        });
        let mut acts = self.activities.lock().unwrap();
        let _ = acts.execute("herdr.agent_prompt_nowait", &req);
        Ok(Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn agent_prompt_wait_background(
        &self,
        _name: &str,
        _prompt: &str,
        _timeout_ms: u32,
        _log_file: std::fs::File,
    ) -> std::io::Result<Child> {
        unimplemented!()
    }
}

#[test]
fn test_mock_activities_herdr_contract() {
    let mut acts = MockActivities::new();
    acts.register(
        "herdr.agent_list",
        ActivityResult::Herdr(HerdrResult::AgentOutput {
            content: r#"{"result":{"agents":[]}}"#.to_string(),
        }),
    );
    acts.register(
        "herdr.agent_get",
        ActivityResult::Herdr(HerdrResult::AgentOutput {
            content: r#"{"result":{"agent":{"pane_id":"%1"}}}"#.to_string(),
        }),
    );
    acts.register(
        "herdr.pane_split",
        ActivityResult::Herdr(HerdrResult::AgentOutput {
            content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
        }),
    );
    acts.register(
        "herdr.agent_start:omp",
        ActivityResult::Herdr(HerdrResult::Spawned {
            name: "sude-1".to_string(),
        }),
    );
    acts.register(
        "herdr.agent_send_keys",
        ActivityResult::Herdr(HerdrResult::KeysSent {
            name: "sude-1".to_string(),
        }),
    );
    let herdr = MockActivitiesHerdr::new(acts);

    assert_eq!(herdr.agent_list().unwrap(), r#"{"result":{"agents":[]}}"#);
    let (_ok, info) = herdr.agent_get("sude-1").unwrap();
    assert!(info.contains("%1"));
    let split_out = herdr.pane_split("/cwd", "cos", &[]).unwrap();
    assert!(split_out.status.success());
    let start_out = herdr.agent_start("sude-1", "omp", "%1", 1000, &[]).unwrap();
    assert!(start_out.status.success());
    let keys_out = herdr.agent_send_keys("sude-1", "esc").unwrap();
    assert!(keys_out.status.success());

    assert_eq!(herdr.call_count(), 5);
    assert!(herdr.has_call("herdr.pane_split"));
    assert_eq!(
        herdr.agent_starts(),
        vec![("sude-1".to_string(), "omp".to_string())]
    );
    assert_eq!(
        herdr.send_keys_calls(),
        vec![("sude-1".to_string(), "esc".to_string())]
    );
}

struct DummyGit;

impl GitOps for DummyGit {
    fn run_in(&self, _dir: &Path, _args: &[&str]) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn run(&self, _args: &[&str]) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn rev_parse_commit(&self, _repo: &Path, _base: &str) -> bool {
        true
    }
    fn worktree_list_porcelain(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
        Ok(vec![])
    }
    fn worktree_add(
        &self,
        _repo: &Path,
        _branch: &str,
        _wt: &Path,
        _base: Option<&str>,
    ) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn branch_exists(&self, _repo: &Path, _branch: &str) -> bool {
        false
    }
    fn worktree_add_existing(
        &self,
        _repo: &Path,
        _branch: &str,
        _wt: &Path,
    ) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn worktree_remove(&self, _repo: &Path, _wt: &Path) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn remote_list(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
        Ok(vec![])
    }
    fn is_core_bare(&self, _repo: &Path) -> bool {
        false
    }
    fn has_core_worktree(&self, _repo: &Path) -> bool {
        false
    }
    fn enable_worktree_config(&self, _repo: &Path) -> std::io::Result<Output> {
        unimplemented!()
    }
    fn set_worktree_pushurl_blocked(&self, _wt: &Path, _remote: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn is_git_dir(&self, _cwd: &Path) -> bool {
        true
    }
    fn diff_and_status(&self, _cwd: &Path) -> (String, String) {
        ("dh".to_string(), "clean".to_string())
    }
}

#[test]
fn test_unknown_harness_rejected_before_split_n4() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-unknown-harness-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    let spy = MockActivitiesHerdr::new(MockActivities::new());
    let dummy_git = DummyGit;

    let mut cfg = config::Config::default();
    cfg.kinds.insert(
        "badkind".to_string(),
        config::Kind {
            harness: "unsupported_harness".to_string(),
            model: "m".to_string(),
            effort: None,
            fallback: None,
            can_review: false,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );

    let opts = parse_spawn_args(&[
        "badkind-task1".to_string(),
        "badkind".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &dummy_git, &spy, quaqua::layout::SpawnOrigin::Cli);
    assert!(res.is_err());
    // N4: pane_split must NEVER have been called!
    assert!(!spy.has_call("herdr.pane_split"));
    assert_eq!(spy.call_count(), 0);
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_missing_effort_rejected_before_split() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-missing-effort-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    let spy = MockActivitiesHerdr::new(MockActivities::new());
    let dummy_git = DummyGit;

    let mut cfg = config::Config::default();
    cfg.kinds.insert(
        "tieude".to_string(),
        config::Kind {
            harness: "opencode".to_string(),
            model: "m".to_string(),
            effort: None,
            fallback: None,
            can_review: false,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );

    let opts = parse_spawn_args(&[
        "tieude-task1".to_string(),
        "tieude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &dummy_git, &spy, quaqua::layout::SpawnOrigin::Cli);
    assert!(res.is_err());
    assert!(!spy.has_call("herdr.pane_split"));
    assert_eq!(spy.call_count(), 0);
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_unsupported_effort_rejected_before_split() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-unsupported-effort-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    let spy = MockActivitiesHerdr::new(MockActivities::new());
    let dummy_git = DummyGit;

    let mut cfg = config::Config::default();
    cfg.kinds.insert(
        "tieude".to_string(),
        config::Kind {
            harness: "opencode".to_string(),
            model: "m".to_string(),
            effort: Some("banana".to_string()),
            fallback: None,
            can_review: false,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );

    let opts = parse_spawn_args(&[
        "tieude-task1".to_string(),
        "tieude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &dummy_git, &spy, quaqua::layout::SpawnOrigin::Cli);
    assert!(res.is_err());
    assert!(!spy.has_call("herdr.pane_split"));
    assert_eq!(spy.call_count(), 0);
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_repro_f1_early_rejection_tests_do_not_write_to_default_runs() {
    let temp = std::env::temp_dir().join(format!("quaqua-f1-ambient-{}", std::process::id()));
    let fake_ws = temp.join("ws");
    let runs_dir = fake_ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();

    // When running early-rejection tests, they must be isolated with their own temp runs
    // and must not touch or create fake_ws/runs/quaqua.db.
    test_unknown_harness_rejected_before_split_n4();
    test_missing_effort_rejected_before_split();
    test_unsupported_effort_rejected_before_split();

    assert!(
        !runs_dir.join("quaqua.db").exists(),
        "isolated tests must not write to ambient/default runs dir"
    );
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_spawn_worker_records_spawn_failed_on_early_rejection_in_production() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-f1-realdk-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    let mut cfg = config::Config::default();
    cfg.kinds.insert(
        "badkind".to_string(),
        config::Kind {
            harness: "unsupported_harness".to_string(),
            model: "m".to_string(),
            effort: None,
            fallback: None,
            can_review: false,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );

    let opts = parse_spawn_args(&[
        "badkind-task1".to_string(),
        "badkind".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let res = quaqua::worker::spawn::spawn_worker(
        opts,
        &cfg,
        &DummyGit,
        &MockActivitiesHerdr::new(MockActivities::new()),
        quaqua::layout::SpawnOrigin::Cli,
    );
    assert!(res.is_err());

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "spawn_failed");
    assert_eq!(events[0].subject.as_deref(), Some("badkind-task1"));
    assert!(events[0].payload_json.contains("unsupported harness"));
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_cap_pipeline_slot_reservation_mock() {
    let temp_dir = std::env::temp_dir().join(format!("quaqua-cap-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let cfg = config::embedded_team_config().expect("embedded team.toml");

    // Write owner files: 1 live worker tieude-prompt (not a reviewer!)
    std::fs::write(temp_dir.join("tieude-prompt.owner"), "cos\n").unwrap();

    // Write pipeline state: finished=0, builder dead, but builder name in live_workers
    std::fs::write(
        temp_dir.join("pipeline-feature.state"),
        "task=feature\nbuilder=sude\nlead=cos\npipeline_pid=99999999\nfinished=0\n",
    )
    .unwrap();

    // Agents list has sude-feature (alive builder) + tieude-prompt
    let mock = MockActivitiesHerdr::with_agent_list(
        r#"{"result":{"agents":[{"name":"sude-feature","agent_status":"working"},{"name":"tieude-prompt","agent_status":"working"}]}}"#,
    );
    std::fs::write(temp_dir.join("sude-feature.owner"), "cos\n").unwrap();
    std::fs::write(temp_dir.join("sude-feature.pipeline"), "feature\n").unwrap();

    // Total slots: 2 live workers (sude-feature + tieude-prompt)
    // Pipeline 'feature' has 1 active worker (sude-feature), so it reserves 2 - 1 = 1 slot.
    // Total should be 2 live + 1 reserved = 3 slots!
    // Under the old bug, tieude-prompt was counted as a pipeline reviewer for 'feature', reserving 0 slots (giving 2).
    let count = count_owner_slots(&temp_dir, "cos", None, &mock, &cfg);
    assert_eq!(count, 3);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_cap_pipeline_rust_state_without_task_line() {
    // A state written by PipelineState::save has no `task=` line for Rust pipelines.
    // The same 2-slot pipeline must not also count its live builder as a standalone worker.
    let temp_dir = std::env::temp_dir().join(format!("quaqua-cap-notask-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let cfg = config::embedded_team_config().expect("embedded team.toml");
    let owner = "quaqua-intake-lead";

    std::fs::write(temp_dir.join("sude-blockedfix.owner"), format!("{owner}\n")).unwrap();
    std::fs::write(temp_dir.join("sude-blockedfix.pipeline"), "blockedfix\n").unwrap();
    std::fs::write(
        temp_dir.join("pipeline-blockedfix.state"),
        format!("repo=/repo\nbrief=/b.md\nbuilder=sude\nreviewer=suhuynh\nreview=auto\nrounds=2\nbase=main\nwt=/repo/.wt/sude-blockedfix\nphase=build\nround=0\nfinished=0\nlead=quaqua-intake-lead\nlead_pane=w1:p1\npipeline_pid={}\n", std::process::id()),
    )
    .unwrap();

    let mock = MockActivitiesHerdr::with_agent_list(
        r#"{"result":{"agents":[{"name":"sude-blockedfix","agent_status":"working"}]}}"#,
    );

    // 1 live builder + (2 - 1 already counted) reserved = 2, not 1 + 2 = 3.
    let count = count_owner_slots(&temp_dir, owner, None, &mock, &cfg);
    assert_eq!(count, 2);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_cap_pipeline_rust_state_review_round() {
    // Builder + fresh reviewer of the same pipeline are its 2 slots, not 2 live + 2 reserved.
    let temp_dir = std::env::temp_dir().join(format!("quaqua-cap-review-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let cfg = config::embedded_team_config().expect("embedded team.toml");
    let owner = "quaqua-intake-lead";

    std::fs::write(temp_dir.join("sude-blockedfix.owner"), format!("{owner}\n")).unwrap();
    std::fs::write(temp_dir.join("sude-blockedfix.pipeline"), "blockedfix\n").unwrap();
    std::fs::write(
        temp_dir.join("suhuynh-blockedfixr1.owner"),
        format!("{owner}\n"),
    )
    .unwrap();
    std::fs::write(
        temp_dir.join("suhuynh-blockedfixr1.pipeline"),
        "blockedfix\n",
    )
    .unwrap();
    std::fs::write(
        temp_dir.join("pipeline-blockedfix.state"),
        format!("repo=/repo\nbuilder=sude\nreviewer=suhuynh\nphase=review\nround=1\nfinished=0\nlead=quaqua-intake-lead\npipeline_pid={}\n", std::process::id()),
    )
    .unwrap();

    let mock = MockActivitiesHerdr::with_agent_list(
        r#"{"result":{"agents":[{"name":"sude-blockedfix","agent_status":"working"},{"name":"suhuynh-blockedfixr1","agent_status":"working"}]}}"#,
    );

    let count = count_owner_slots(&temp_dir, owner, None, &mock, &cfg);
    assert_eq!(count, 2);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_cap_pipeline_state_task_line_wins_over_filename() {
    // Legacy/shell states carry `task=`; an explicit line is the task, not the file name.
    let temp_dir = std::env::temp_dir().join(format!("quaqua-cap-taskline-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let cfg = config::embedded_team_config().expect("embedded team.toml");
    let owner = "cos";

    std::fs::write(temp_dir.join("sude-other.owner"), "cos\n").unwrap();
    std::fs::write(temp_dir.join("sude-other.pipeline"), "other\n").unwrap();
    std::fs::write(
        temp_dir.join("pipeline-blockedfix.state"),
        format!(
            "task=other\nbuilder=sude\nlead=cos\nfinished=0\npipeline_pid={}\n",
            std::process::id()
        ),
    )
    .unwrap();

    let mock = MockActivitiesHerdr::with_agent_list(
        r#"{"result":{"agents":[{"name":"sude-other","agent_status":"working"}]}}"#,
    );
    assert_eq!(count_owner_slots(&temp_dir, owner, None, &mock, &cfg), 2);

    std::fs::write(temp_dir.join("sude-blockedfix.owner"), "cos\n").unwrap();
    std::fs::write(temp_dir.join("sude-blockedfix.pipeline"), "blockedfix\n").unwrap();
    std::fs::write(
        temp_dir.join("pipeline-blockedfix.state"),
        format!(
            "task=other\nbuilder=sude\nlead=cos\nfinished=0\npipeline_pid={}\n",
            std::process::id()
        ),
    )
    .unwrap();
    let mock = MockActivitiesHerdr::with_agent_list(
        r#"{"result":{"agents":[{"name":"sude-blockedfix","agent_status":"working"}]}}"#,
    );
    // Not part of pipeline 'other': 1 live + full 2 reservation.
    assert_eq!(count_owner_slots(&temp_dir, owner, None, &mock, &cfg), 3);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_cap_count_ignores_process_env_team_toml() {
    // Repro: another test in this binary points QUAQUA_TEAM_TOML at a config
    // whose kinds are not the default set. Slot counting must not let that
    // env decide who is a task worker, and a failed load must not silently
    // demote every worker to a non-worker (which skips the pipeline reserve).
    let _env_lock = lock_env();
    let temp_dir =
        std::env::temp_dir().join(format!("quaqua-cap-envfree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).unwrap();
    let owner = "cos";
    let cfg = config::embedded_team_config().expect("embedded team.toml");

    std::fs::write(temp_dir.join("sude-blockedfix.owner"), "cos\n").unwrap();
    std::fs::write(temp_dir.join("sude-blockedfix.pipeline"), "blockedfix\n").unwrap();
    std::fs::write(
        temp_dir.join("pipeline-blockedfix.state"),
        format!(
            "task=other\nbuilder=sude\nlead=cos\nfinished=0\npipeline_pid={}\n",
            std::process::id()
        ),
    )
    .unwrap();

    let mock = MockActivitiesHerdr::with_agent_list(
        r#"{"result":{"agents":[{"name":"sude-blockedfix","agent_status":"working"}]}}"#,
    );

    // A valid config that only knows kind `ni`, plus an override that cannot
    // be loaded at all. Neither may change the count: 1 live standalone
    // worker + the 2-slot pipeline reserve.
    let foreign = temp_dir.join("foreign-team.toml");
    std::fs::write(
        &foreign,
        "schema_version = 2\n\n[kinds.ni]\nharness = \"claude\"\nmodel = \"m\"\neffort = \"low\"\n",
    )
    .unwrap();

    for override_path in [foreign.as_path(), Path::new("/nonexistent/quaqua-team.toml")] {
        std::env::set_var("QUAQUA_TEAM_TOML", override_path);
        assert_eq!(
            count_owner_slots(&temp_dir, owner, None, &mock, &cfg),
            3,
            "QUAQUA_TEAM_TOML={} must not change the slot count",
            override_path.display()
        );
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
}
#[test]
fn test_cap_pipeline_double_count_repro() {
    // Repro for Cause (c): pipeline gridown2 has a live builder tieude-gridown2
    // (e.g. after a fallback or non-default builder) without .pipeline sidecar.
    // Must count as 2 slots (the pipeline once), not 3 (builder 1 + reserve 2).
    let temp_dir =
        std::env::temp_dir().join(format!("quaqua-cap-doublecount-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let cfg = config::embedded_team_config().expect("embedded team.toml");
    let owner = "quaqua-rs-lead";

    std::fs::write(temp_dir.join("tieude-gridown2.owner"), format!("{owner}\n")).unwrap();
    // No tieude-gridown2.pipeline file written
    std::fs::write(
        temp_dir.join("pipeline-gridown2.state"),
        format!("repo=/repo\nbuilder=sude\nphase=build\nround=1\nfinished=0\nlead=quaqua-rs-lead\npipeline_pid={}\n", std::process::id()),
    )
    .unwrap();

    let mock = MockActivitiesHerdr::with_agent_list(
        r#"{"result":{"agents":[{"name":"tieude-gridown2","agent_status":"working"}]}}"#,
    );

    let count = count_owner_slots(&temp_dir, owner, None, &mock, &cfg);
    assert_eq!(
        count, 2,
        "a pipeline must count once (2 slots), never double-count to 3"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}
#[test]
fn test_cost_budget_parser_and_reader() {
    use quaqua::worker::prompt::{brief_cost_budget, read_worker_cost};

    let b1 = "Goal: test\nHard constraints: none\nCurrent design: none\nCost budget: $0.001\nTime budget: 5 min";
    assert_eq!(brief_cost_budget(b1), Some(0.001));

    let b2 = "Goal: test\nCost budget: $2.50\n";
    assert_eq!(brief_cost_budget(b2), Some(2.5));

    let b3 = "Goal: test\nCost budget: 0.75\n";
    assert_eq!(brief_cost_budget(b3), Some(0.75));

    let b4 = "Goal: test\nNo cost line";
    assert_eq!(brief_cost_budget(b4), None);

    // Cost reader via PROMPT_WORKER_COST_FILE fixture
    let tmp = std::env::temp_dir().join(format!("cost-fixture-{}", std::process::id()));
    std::fs::write(&tmp, "0.4242\n").unwrap();
    std::env::set_var("PROMPT_WORKER_COST_FILE", &tmp);
    let cost = read_worker_cost("tieude-test", "ses_123", "/tmp", "opencode");
    assert_eq!(cost, Some(0.4242));
    std::env::remove_var("PROMPT_WORKER_COST_FILE");
    let _ = std::fs::remove_file(&tmp);

    // Non-opencode without a matching omp session dir returns None
    assert_eq!(
        read_worker_cost("sude-test", "ses_123", "/nonexistent/omp-worktree", "omp"),
        None
    );
}

#[test]
fn test_runs_dir_resolution_n5() {
    // N5: no process-global set_var mutations
    let test_home = Path::new("target/test_home");
    assert_eq!(
        resolve_home_quaqua_runs(Some(test_home)),
        test_home.join("quaqua/runs")
    );
    assert_eq!(
        resolve_default_runs_dir(None, Some(test_home)),
        test_home.join("quaqua/runs")
    );

    let override_dir = Path::new("target/override_runs");
    assert_eq!(
        resolve_default_runs_dir(Some(override_dir), Some(test_home)),
        override_dir.to_path_buf()
    );
    assert_eq!(
        resolve_home_quaqua_runs(Some(test_home)),
        test_home.join("quaqua/runs")
    );

    // workspace_root: QUAQUA_HOME wins, else $HOME/quaqua
    let quaqua_home = Path::new("target/quaqua_home");
    assert_eq!(
        resolve_workspace_root(Some(quaqua_home), Some(test_home)),
        quaqua_home.to_path_buf()
    );
    assert_eq!(
        resolve_workspace_root(None, Some(test_home)),
        test_home.join("quaqua")
    );
}

#[test]
fn test_team_toml_harness_routing() {
    let team_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates/workspace/team.toml");
    let cfg = config::load(&team_path).expect("valid team.toml");

    // tieude is omp with the opencode-go model at effort high
    assert_eq!(cfg.kinds["tieude"].harness, "omp");
    assert_eq!(cfg.kinds["tieude"].model, "opencode-go/deepseek-v4.1-flash");
    assert_eq!(cfg.kinds["tieude"].effort.as_deref(), Some("high"));
    assert!(!cfg.kinds["tieude"].can_review);
    // fallback seeds: tieude->sude, sude->suhuynh; claude kinds none
    assert_eq!(cfg.kinds["tieude"].fallback.as_deref(), Some("sude"));
    assert_eq!(cfg.kinds["sude"].fallback.as_deref(), Some("suhuynh"));
    for k in ["ni", "suhuynh", "truonglao"] {
        assert_eq!(cfg.kinds[k].fallback, None, "{k} must have no fallback");
    }

    // sude is omp with antigravity gemini high
    assert_eq!(cfg.kinds["sude"].harness, "omp");
    assert_eq!(
        cfg.kinds["sude"].model,
        "google-antigravity/gemini-3.8-flash"
    );
    assert_eq!(cfg.kinds["sude"].effort.as_deref(), Some("high"));
    // can_review = true since PR #66 (the repo config, not a policy claim: the kind can
    // back a reviewer role).
    assert!(cfg.kinds["sude"].can_review);

    // ni is claude with can_review=true
    assert_eq!(cfg.kinds["ni"].harness, "claude");
    assert_eq!(cfg.kinds["ni"].model, "opus-5.5");
    assert!(cfg.kinds["ni"].can_review);

    // suhuynh is the claude reviewer at medium, a different harness from the omp builders
    assert_eq!(cfg.kinds["suhuynh"].harness, "claude");
    assert_eq!(cfg.kinds["suhuynh"].model, "opus-5.5");
    assert_eq!(cfg.kinds["suhuynh"].effort.as_deref(), Some("medium"));
    assert!(cfg.kinds["suhuynh"].can_review);

    // truonglao is claude with can_review=true; every kind is omp/claude
    assert_eq!(cfg.kinds["truonglao"].harness, "claude");
    assert!(cfg.kinds["truonglao"].can_review);
    assert_eq!(cfg.kinds.len(), 5, "only the five surviving kinds");
    assert!(cfg
        .kinds
        .values()
        .all(|k| ["omp", "claude"].contains(&k.harness.as_str())));

    // every purpose resolves to a surviving kind (research left detu behind)
    assert_eq!(cfg.purposes["research"], "tieude");
    assert!(cfg.purposes.values().all(|k| cfg.kinds.contains_key(k)));

    // strategy.lead_kind defaults to ni (Lead on claude · opus-5.5 · low)
    assert_eq!(cfg.strategy.lead_kind.as_deref(), Some("ni"));
}

#[test]
fn test_lead_launch_uses_strategy_lead_kind_and_override() {
    let team_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates/workspace/team.toml");
    let cfg = config::load(&team_path).expect("valid team.toml");

    // 1. Default lead launch uses strategy.lead_kind ("ni" on claude)
    let (kind, launch) = quaqua::lead::spawn::resolve_lead_launch(&cfg, None).expect("lead launch");
    assert_eq!(kind, "ni");
    assert_eq!(launch.harness, "claude");
    assert_eq!(launch.model, "opus-5.5");
    assert_eq!(launch.effort, "low");

    let cmd = build_launch_cmd(
        &launch,
        "quaqua-rs-lead",
        "/runs/test.rules.md",
        "/runs",
        "/repo",
        false,
    )
    .unwrap();
    assert!(cmd.split_envs.is_empty());
    assert_eq!(
        cmd.agent_args,
        vec![
            "--",
            "--model",
            "claude-opus-5-5",
            "--effort",
            "low",
            "--append-system-prompt-file",
            "/runs/test.rules.md",
        ]
    );

    // 2. LEAD_KIND env override switches lead to sude (omp)
    let (kind_sude, launch_sude) =
        quaqua::lead::spawn::resolve_lead_launch(&cfg, Some("sude")).expect("sude lead");
    assert_eq!(kind_sude, "sude");
    assert_eq!(launch_sude.harness, "omp");
    assert_eq!(launch_sude.model, "google-antigravity/gemini-3.8-flash");
    assert_eq!(launch_sude.effort, "high");

    let cmd_sude = build_launch_cmd(
        &launch_sude,
        "quaqua-rs-lead",
        "/runs/test.rules.md",
        "/runs",
        "/repo",
        false,
    )
    .unwrap();
    assert!(cmd_sude.split_envs.is_empty());
    assert_eq!(
        cmd_sude.agent_args,
        vec![
            "--",
            "--model",
            "google-antigravity/gemini-3.8-flash",
            "--thinking",
            "high",
            "--auto-approve",
            "--append-system-prompt",
            "/runs/test.rules.md",
        ]
    );

    // 3. LEAD_KIND env override switches lead back to tieude (omp)
    let (kind_tieude, launch_tieude) =
        quaqua::lead::spawn::resolve_lead_launch(&cfg, Some("tieude")).expect("tieude lead");
    assert_eq!(kind_tieude, "tieude");
    assert_eq!(launch_tieude.harness, "omp");
    assert_eq!(launch_tieude.model, "opencode-go/deepseek-v4.1-flash");
    assert_eq!(launch_tieude.effort, "high");

    let cmd_tieude = build_launch_cmd(
        &launch_tieude,
        "quaqua-rs-lead",
        "/runs/test.rules.md",
        "/runs",
        "/repo",
        false,
    )
    .unwrap();
    assert!(cmd_tieude.split_envs.is_empty());
    assert_eq!(
        cmd_tieude.agent_args,
        vec![
            "--",
            "--model",
            "opencode-go/deepseek-v4.1-flash",
            "--thinking",
            "high",
            "--auto-approve",
            "--append-system-prompt",
            "/runs/test.rules.md",
        ]
    );

    // 4. Unknown LEAD_KIND falls back to tieude
    let (kind_fallback, launch_fallback) =
        quaqua::lead::spawn::resolve_lead_launch(&cfg, Some("nonexistent_kind")).expect("fallback");
    assert_eq!(kind_fallback, "tieude");
    assert_eq!(launch_fallback.harness, "omp");

    // 5. Missing lead_kind in Strategy defaults to tieude
    let mut cfg_no_lead = cfg.clone();
    cfg_no_lead.strategy.lead_kind = None;
    let (kind_default, launch_default) =
        quaqua::lead::spawn::resolve_lead_launch(&cfg_no_lead, None).expect("default");
    assert_eq!(kind_default, "tieude");
    assert_eq!(launch_default.harness, "omp");
}

#[test]
fn test_spawn_args_parsing() {
    let args = vec![
        "--base".to_string(),
        "main".to_string(),
        "--review".to_string(),
        "suhuynh-rev1".to_string(),
        "suhuynh".to_string(),
        "/repo".to_string(),
    ];
    let opts = parse_spawn_args(&args).expect("valid args");
    assert_eq!(opts.base.as_deref(), Some("main"));
    assert!(opts.review);
    assert_eq!(opts.name, "suhuynh-rev1");
    assert_eq!(opts.kind.as_deref(), Some("suhuynh"));

    // Trailing base without value
    let bad = vec![
        "suhuynh-rev1".to_string(),
        "suhuynh".to_string(),
        "/repo".to_string(),
        "--base".to_string(),
    ];
    assert!(parse_spawn_args(&bad).is_err());
}
#[test]
fn test_spawn_args_parsing_with_purpose() {
    // --purpose with 2 positional args (name and repo)
    let args = vec![
        "--purpose".to_string(),
        "review".to_string(),
        "suhuynh-rev1".to_string(),
        "/repo".to_string(),
    ];
    let opts = parse_spawn_args(&args).expect("valid args with purpose");
    assert_eq!(opts.name, "suhuynh-rev1");
    assert_eq!(opts.purpose.as_deref(), Some("review"));
    assert_eq!(opts.kind, None);
    assert_eq!(opts.repo_path, PathBuf::from("/repo"));

    // --purpose= syntax with 3 positional args
    let args2 = vec![
        "--purpose=build".to_string(),
        "sude-build1".to_string(),
        "sude".to_string(),
        "/repo".to_string(),
    ];
    let opts2 = parse_spawn_args(&args2).expect("valid args with purpose=");
    assert_eq!(opts2.name, "sude-build1");
    assert_eq!(opts2.purpose.as_deref(), Some("build"));
    assert_eq!(opts2.kind.as_deref(), Some("sude"));
}


#[test]
fn test_cli_cap_smoke() {
    let bin = env!("CARGO_BIN_EXE_quaqua");
    let tmp = std::env::temp_dir().join(format!("quaqua-cap-cli-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let team = tmp.join("team.toml");
    std::fs::write(&team, "schema_version = 2\n").unwrap();
    let runs = tmp.join("runs");
    let _ = std::fs::create_dir_all(&runs);
    let out = Command::new(bin)
        .arg("cap")
        .env("QUAQUA_TEAM_TOML", &team)
        .env("QUAQUA_RUNS_DIR", &runs)
        .output()
        .expect("runs quaqua cap");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.trim().parse::<i64>().is_ok());

    // A team.toml that cannot load must surface, never silently count as if
    // every worker were a non-task worker.
    let bad = Command::new(bin)
        .arg("cap")
        .env("QUAQUA_TEAM_TOML", tmp.join("missing-team.toml"))
        .env("QUAQUA_RUNS_DIR", &runs)
        .output()
        .expect("runs quaqua cap");
    assert!(!bad.status.success());
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(err.contains("error:"), "stderr: {err}");

    let _ = std::fs::remove_dir_all(&tmp);
}
#[test]
fn test_taskforce_dual_lens_cap_reservation() {
    let tmp = std::env::temp_dir().join(format!("quaqua-cap-tf-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let cfg = config::embedded_team_config().expect("embedded team.toml");
    let mock = MockActivitiesHerdr::with_agent_list(r#"{"result":{"agents":[]}}"#);

    // Zero workers, no state -> 0
    assert_eq!(count_owner_slots(&tmp, "cos", None, &mock, &cfg), 0);

    // Active dual-lens state with current PID (alive) -> reserves 2 slots
    let state_file = tmp.join("taskforce-mytest.state");
    std::fs::write(
        &state_file,
        format!(
            "task=mytest\nkind=dual-lens\nowner=cos\nfinished=0\ntaskforce_pid={}\n",
            std::process::id()
        ),
    )
    .unwrap();

    assert_eq!(count_owner_slots(&tmp, "cos", None, &mock, &cfg), 2);

    // Partial membership still reserves the whole weight: one live member is
    // 1 live + 1 reserved = 2, and two live members are 2 live = 2 — never 3.
    let members = ["truonglao-mytestdl1", "suhuynh-mytestdl2"];
    for (i, name) in members.iter().enumerate() {
        std::fs::write(tmp.join(format!("{name}.owner")), "cos\n").unwrap();
        std::fs::write(tmp.join(format!("{name}.taskforce")), "mytest\n").unwrap();
        let live = &members[..=i];
        let herdr = herdr_listing(&agent_list_json(live));
        assert_eq!(
            count_owner_slots(&tmp, "cos", None, &herdr, &cfg),
            2,
            "{} live dual-lens member(s) must still account for 2 slots",
            live.len()
        );
    }
    for name in members {
        std::fs::remove_file(tmp.join(format!("{name}.owner"))).unwrap();
        std::fs::remove_file(tmp.join(format!("{name}.taskforce"))).unwrap();
    }

    // An active dual-lens plus cap-2 standalone workers fills the owner's cap.
    let cap = quaqua::cap::owner_cap(&cfg, "cos");
    let weight = cfg.limits.weights.get("dual-lens").copied().unwrap_or(2);
    let solo: Vec<String> = (1..=(cap - weight)).map(|i| format!("sude-w{i}")).collect();
    let mut names: Vec<&str> = solo.iter().map(|w| w.as_str()).collect();
    names.extend(members);
    for w in &solo {
        std::fs::write(tmp.join(format!("{w}.owner")), "cos\n").unwrap();
    }
    let herdr = herdr_listing(&agent_list_json(&names));
    assert_eq!(
        count_owner_slots(&tmp, "cos", None, &herdr, &cfg),
        cap,
        "a dual-lens taskforce counts {weight} slots, not 1"
    );
    for w in &solo {
        std::fs::remove_file(tmp.join(format!("{w}.owner"))).unwrap();
    }

    // Starting one is refused when the owner cannot find the free slots.
    assert!(quaqua::cap::check_owner_cap(&cfg, "cos", cap - weight, weight, "dual-lens").is_ok());
    let err =
        quaqua::cap::check_owner_cap(&cfg, "cos", cap - weight + 1, weight, "dual-lens").unwrap_err();
    assert!(err.contains("worker cap exceeded"), "{err}");

    // Finished dual-lens -> 0 slots reserved
    std::fs::write(
        &state_file,
        format!(
            "task=mytest\nkind=dual-lens\nowner=cos\nfinished=1\ntaskforce_pid={}\n",
            std::process::id()
        ),
    )
    .unwrap();
    assert_eq!(count_owner_slots(&tmp, "cos", None, &mock, &cfg), 0);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_pipeline_blockers_none_exemption() {
    use quaqua::pipeline::has_real_blockers;

    assert!(!has_real_blockers("## Blockers\n- none\n"));
    assert!(!has_real_blockers("## Blockers\nnone\n"));
    assert!(!has_real_blockers("## Blockers\n* none\n"));
    assert!(!has_real_blockers("## Blockers\n- None\n"));
    assert!(!has_real_blockers("## Blockers\n- None…\n"));
    assert!(!has_real_blockers("## Blockers\n- None.\n"));
    assert!(!has_real_blockers("## Blockers\n- None...\n"));
    assert!(!has_real_blockers("## Blockers\nNone…\n"));
    assert!(!has_real_blockers("## Blockers\nNone.\n"));
    assert!(!has_real_blockers("## Blockers\n* none…\n"));
    assert!(!has_real_blockers(
        "## Blockers\n\n- none\n\n## Lessons\n- none\n"
    ));
    assert!(!has_real_blockers("Just plan text\n\n## Lessons\n- none\n"));
    // Informational notes after None must not be treated as blockers
    assert!(!has_real_blockers("## Blockers\n- None. All done...\n"));
    assert!(!has_real_blockers(
        "## Blockers\n- None. All 9 tests passed.\n"
    ));
    assert!(!has_real_blockers("## Blockers\n- **None**. All done\n"));
    assert!(!has_real_blockers("## Blockers\n**None**. All done...\n"));
    assert!(!has_real_blockers("## Blockers\n- None... all clear\n"));

    // Real blockers must return true
    assert!(has_real_blockers(
        "## Blockers\n- cannot plan without the spec\n"
    ));
    assert!(has_real_blockers(
        "## Blockers\n- implementation incomplete\n"
    ));
    assert!(has_real_blockers(
        "## Blockers\n- none\n- also a real blocker\n"
    ));
    assert!(has_real_blockers("## Blockers\n- None of the tests pass\n"));
    assert!(has_real_blockers(
        "## Blockers\n- none: cannot connect to db\n"
    ));
    assert!(has_real_blockers(
        "## Blockers\n- None - build compilation failed\n"
    ));
}

#[test]
fn test_pipeline_blocked_keeps_finished_zero() {
    use quaqua::pipeline::pipeline_finished;

    // A blocked stop stays resumable: finished must not be set.
    assert!(!pipeline_finished("blocked", 0));
    assert!(!pipeline_finished("blocked", 3));
    // Real outcomes stay final.
    assert!(pipeline_finished("clean", 0));
    assert!(pipeline_finished("disputed", 0));
    // Non-zero exits never finish the state, whatever the status says.
    assert!(!pipeline_finished("plan-ready", 5));
    assert!(!pipeline_finished("clean", 4));
}

#[test]
fn test_pipeline_rounds_raise_only_on_resume() {
    let mut state = quaqua::pipeline::PipelineState::default();
    state.rounds = 3;
    let resume_rounds: Option<u32> = Some(2);
    if let Some(r) = resume_rounds {
        if r > state.rounds {
            state.rounds = r;
        }
    }
    assert_eq!(
        state.rounds, 3,
        "resuming with lower rounds must not lower the cap"
    );

    let resume_rounds_higher: Option<u32> = Some(5);
    if let Some(r) = resume_rounds_higher {
        if r > state.rounds {
            state.rounds = r;
        }
    }
    assert_eq!(
        state.rounds, 5,
        "resuming with higher rounds must raise the cap"
    );
}

#[test]
fn test_pipeline_outcome_no_changes_detection() {
    let content = "Outcome: no changes\n\n## Lessons\n- none\n";
    let is_no_changes = content.lines().any(|line| {
        let t = line.trim();
        if let Some(rest) = t
            .strip_prefix("Outcome:")
            .or_else(|| t.strip_prefix("outcome:"))
        {
            let rest_upper = rest.trim().to_ascii_uppercase();
            rest_upper.starts_with("NO CHANGES")
                || rest_upper.starts_with("NO-OP")
                || rest_upper.starts_with("NO OP")
        } else {
            false
        }
    });
    assert!(is_no_changes);

    let content_changes = "Outcome: modified 2 files\n\n## Lessons\n- none\n";
    let is_no_changes = content_changes.lines().any(|line| {
        let t = line.trim();
        if let Some(rest) = t
            .strip_prefix("Outcome:")
            .or_else(|| t.strip_prefix("outcome:"))
        {
            let rest_upper = rest.trim().to_ascii_uppercase();
            rest_upper.starts_with("NO CHANGES")
                || rest_upper.starts_with("NO-OP")
                || rest_upper.starts_with("NO OP")
        } else {
            false
        }
    });
    assert!(!is_no_changes);
}

#[test]
fn test_strategy_default_reviewer_selection() {
    use quaqua::pipeline::select_default_reviewer;

    let toml_str = r#"
schema_version = 2
[strategy]
default_kind = "tieude"
default_reviewer = ["suhuynh", "ni"]
builder_reviewer_differ = "harness"
[kinds.sude]
harness = "omp"
model = "google-antigravity/gemini-3.8-flash"
can_review = false
[kinds.suhuynh]
harness = "claude"
model = "opus-5.5"
can_review = true
[kinds.ni]
harness = "claude"
model = "opus-5.5"
can_review = true
"#;
    let cfg: quaqua::config::Config = toml::from_str(toml_str).unwrap();

    // 1. With builder sude (omp), select_default_reviewer selects suhuynh
    let sel1 = select_default_reviewer("sude", &cfg).unwrap();
    assert_eq!(sel1, "suhuynh");

    // 2. When suhuynh is unavailable (via QUAQUA_UNAVAILABLE_REVIEWERS probe), falls back to ni
    std::env::set_var("QUAQUA_UNAVAILABLE_REVIEWERS", "suhuynh");
    let sel_fallback = select_default_reviewer("sude", &cfg).unwrap();
    std::env::remove_var("QUAQUA_UNAVAILABLE_REVIEWERS");
    assert_eq!(sel_fallback, "ni");
}

/// SLP: a project Lead spawns at most `[roles.lead] max_peers` (default 4) Peers;
/// the CoS keeps the global `[limits] max_workers_per_orchestrator`. Every spawn
/// path checks the same gate, so a refusal names the cap that fired.
#[test]
fn test_lead_peer_cap_holds_while_cos_keeps_the_global_cap() {
    use quaqua::cap::{check_owner_cap, owner_cap};

    let toml_str = r#"
schema_version = 2
[limits]
max_workers_per_orchestrator = 8
weights = { pipeline = 2, "dual-lens" = 2 }
"#;
    // An older team.toml with no [roles] and no max_peers still caps a Lead at 4.
    let cfg: quaqua::config::Config = toml::from_str(toml_str).unwrap();
    assert_eq!(owner_cap(&cfg, "arch-lead"), 4);
    assert_eq!(owner_cap(&cfg, "cos"), 8);

    // 4 Peers is the limit: the 4th spawn fits, the 5th is refused, naming the cap.
    assert!(check_owner_cap(&cfg, "arch-lead", 3, 1, "this spawn").is_ok());
    let err = check_owner_cap(&cfg, "arch-lead", 4, 1, "this spawn").unwrap_err();
    assert!(err.contains("worker cap exceeded"), "{err}");
    assert!(err.contains("owner 'arch-lead'"), "{err}");
    assert!(err.contains("roles.lead.max_peers is 4"), "{err}");

    // A pipeline weighs 2 slots, so a Lead with 3 Peers cannot start one.
    assert!(check_owner_cap(&cfg, "arch-lead", 2, 2, "the pipeline").is_ok());
    assert!(check_owner_cap(&cfg, "arch-lead", 3, 2, "the pipeline").is_err());

    // The CoS is untouched by the Lead cap: 8 slots, the 9th is refused.
    assert!(check_owner_cap(&cfg, "cos", 7, 1, "this spawn").is_ok());
    let err = check_owner_cap(&cfg, "cos", 8, 1, "this spawn").unwrap_err();
    assert!(err.contains("limits.max_workers_per_orchestrator is 8"), "{err}");

    // A project override only tightens: it lowers the Lead cap with the global one.
    let tight: quaqua::config::Config = toml::from_str(
        r#"
schema_version = 2
[limits]
max_workers_per_orchestrator = 8
[roles.lead]
max_peers = 4
[projects.zimage]
max_workers_per_orchestrator = 3
"#,
    )
    .unwrap();
    let resolved = quaqua::config::resolve(tight, Some("zimage")).unwrap();
    assert_eq!(owner_cap(&resolved, "arch-lead"), 3);
    assert_eq!(owner_cap(&resolved, "cos"), 3);
}

#[test]
fn test_lead_max_peers_overrides_the_default() {
    use quaqua::cap::{check_owner_cap, owner_cap};

    let toml_str = r#"
schema_version = 2
[limits]
max_workers_per_orchestrator = 8
[roles.lead]
max_peers = 2
"#;
    let cfg: quaqua::config::Config = toml::from_str(toml_str).unwrap();
    assert_eq!(owner_cap(&cfg, "baro-lead"), 2);
    assert!(check_owner_cap(&cfg, "baro-lead", 1, 1, "this spawn").is_ok());
    let err = check_owner_cap(&cfg, "baro-lead", 2, 1, "this spawn").unwrap_err();
    assert!(err.contains("roles.lead.max_peers is 2"), "{err}");
}

#[test]
fn test_pipeline_project_override_cap() {
    let toml_str = r#"
schema_version = 2
[limits]
max_workers_per_orchestrator = 4
weights = { pipeline = 2 }
[projects.zimage]
max_workers_per_orchestrator = 3
"#;
    let base_cfg: quaqua::config::Config = toml::from_str(toml_str).unwrap();
    let resolved = quaqua::config::resolve(base_cfg.clone(), Some("zimage")).unwrap();
    assert_eq!(resolved.limits.max_workers_per_orchestrator, 3);

    let pipe_weight = resolved
        .limits
        .weights
        .get("pipeline")
        .copied()
        .unwrap_or(2);
    let slots_used = 2; // owner has 2 slots in use
                        // For standard project (cap 4): 2 + 2 <= 4 -> ok
    assert!(slots_used + pipe_weight <= base_cfg.limits.max_workers_per_orchestrator);
    // For zimage (cap 3): 2 + 2 > 3 -> refused
    assert!(slots_used + pipe_weight > resolved.limits.max_workers_per_orchestrator);
}

#[test]
fn test_sude_harness_mapped_to_omp() {
    use quaqua::pipeline::harness_of_kind;
    let toml_str = r#"
        schema_version = 1

        [kinds.sude]
        harness = "omp"
        model = "google-antigravity/gemini-3.8-flash"
        effort = "high"
        fallback = "detu"
        can_review = false
    "#;
    let cfg: quaqua::config::Config = toml::from_str(toml_str).unwrap();
    assert_eq!(harness_of_kind("sude", &cfg), Some("omp"));
}

#[test]
fn test_claude_review_mode_verification() {
    use quaqua::worker::spawn::parse_spawn_args;

    let args = vec![
        "--review".to_string(),
        "suhuynh-testrev".to_string(),
        "suhuynh".to_string(),
        ".".to_string(),
    ];
    let opts = parse_spawn_args(&args).unwrap();
    assert!(opts.review);
    assert_eq!(opts.kind.as_deref(), Some("suhuynh"));
}

fn kind(harness: &str, model: &str, effort: Option<&str>) -> config::Kind {
    config::Kind {
        harness: harness.to_string(),
        model: model.to_string(),
        effort: effort.map(str::to_string),
        fallback: None,
        can_review: false,
        spawn: config::SPAWN_AUTO.to_string(),
        on_cost_cap: config::COST_CAP_WARN.to_string(),
    }
}

fn resolve_launch(kind: &config::Kind, kind_name: &str) -> Result<Launch, (i32, String)> {
    let cfg = config::Config::default();
    quaqua::launch::resolve_launch(kind, kind_name, &cfg)
}
#[test]
fn test_launch_args_carry_exact_model_and_effort() {
    // opencode: model+effort travel as `provider/model#variant` in the pane env —
    // the interactive TUI lists no --model flag (`opencode --help`), only
    // --standalone/--server/--auto/--continue/--session/--prompt.
    let oc = resolve_launch(
        &kind("opencode", "opencode-go/deepseek-v4.1-flash", Some("max")),
        "tieude",
    )
    .unwrap();
    assert_eq!(
        oc,
        Launch {
            harness: "opencode".into(),
            model: "opencode-go/deepseek-v4.1-flash".into(),
            effort: "max".into(),
        }
    );
    let cmd = build_launch_cmd(
        &oc,
        "tieude-test",
        "/r/x.rules.md",
        "~/quaqua/runs",
        "/repo/.wt/x",
        false,
    )
    .unwrap();
    assert_eq!(cmd.split_envs.len(), 2);
    assert_eq!(cmd.split_envs[0].0, "OPENCODE_CONFIG_CONTENT");
    assert_eq!(
        cmd.split_envs[0].1,
        r#"{"model":{"providerID":"opencode-go","model":"deepseek-v4.1-flash","variant":"max"},"instructions":["/r/x.rules.md"]}"#
    );
    assert_eq!(cmd.split_envs[1].0, "XDG_STATE_HOME");
    assert_eq!(
        cmd.split_envs[1].1,
        "~/quaqua/runs/.opencode-state/tieude-test"
    );

    // opencode carries "low" variant as object
    let oc_low = resolve_launch(
        &kind("opencode", "opencode-go/deepseek-v4.1-flash", Some("low")),
        "tieude",
    )
    .unwrap();
    let cmd_low = build_launch_cmd(
        &oc_low,
        "tieude-low",
        "/r/x.rules.md",
        "~/quaqua/runs",
        "/repo/.wt/x",
        false,
    )
    .unwrap();
    assert_eq!(
        cmd_low.split_envs[0].1,
        r#"{"model":{"providerID":"opencode-go","model":"deepseek-v4.1-flash","variant":"low"},"instructions":["/r/x.rules.md"]}"#
    );
    assert_eq!(cmd.agent_args, vec!["--", "--standalone", "--auto"]);
    assert_eq!(cmd_low.split_envs[1].0, "XDG_STATE_HOME");
    assert_eq!(
        cmd_low.split_envs[1].1,
        "~/quaqua/runs/.opencode-state/tieude-low"
    );

    // omp: `--model` + `--thinking` (`omp --help`).
    let omp = resolve_launch(
        &kind("omp", "google-antigravity/gemini-3.8-flash", Some("high")),
        "sude",
    )
    .unwrap();
    let cmd = build_launch_cmd(
        &omp,
        "sude-test",
        "/r/x.rules.md",
        "~/quaqua/runs",
        "/repo/.wt/x",
        false,
    )
    .unwrap();
    assert!(cmd.split_envs.is_empty(), "omp takes no pane env");
    let joined = cmd.agent_args.join(" ");
    assert!(
        joined.contains("--model google-antigravity/gemini-3.8-flash"),
        "{joined}"
    );
    assert!(joined.contains("--thinking high"), "{joined}");

    // claude: `--model` (mapped to claude-*) + `--effort` (`claude --help`).
    let cl = resolve_launch(&kind("claude", "opus-5.5", Some("high")), "truonglao").unwrap();
    let cmd = build_launch_cmd(
        &cl,
        "truonglao-test",
        "/r/x.rules.md",
        "~/quaqua/runs",
        "/repo/.wt/x",
        true,
    )
    .unwrap();
    let joined = cmd.agent_args.join(" ");
    assert!(joined.contains("--model claude-opus-5-5"), "{joined}");
    assert!(joined.contains("--effort high"), "{joined}");
    // review keeps its read-only boundary
    assert!(joined.contains("--permission-mode dontAsk"), "{joined}");
}

#[test]
fn test_launch_args_pi_exact_and_isolated() {
    let pi = resolve_launch(
        &kind("pi", "opencode-go/deepseek-v4.1-flash", Some("max")),
        "tieude",
    )
    .unwrap();
    let home = std::env::var("HOME").unwrap_or_default();
    let ext = format!("{home}/.pi/agent/extensions/herdr-agent-state.ts");
    let build = build_launch_cmd(&pi, "tieude-x", "/r/x.rules.md", "/runs", "/repo/.wt/x", false)
        .unwrap();
    assert!(build.split_envs.is_empty(), "pi takes no pane env");
    assert_eq!(
        build.agent_args,
        vec![
            "--",
            "--model",
            "opencode-go/deepseek-v4.1-flash",
            "--thinking",
            "max",
            "--append-system-prompt",
            "/r/x.rules.md",
            "--session-dir",
            "/runs/.pi-sessions/tieude-x",
            "--no-extensions",
            "-e",
            ext.as_str(),
            "--no-approve",
        ]
    );
    // review: tool allowlist without edit/write
    let review = build_launch_cmd(&pi, "suhuynh-x", "/r/x.rules.md", "/runs", "/repo/.wt/x", true)
        .unwrap();
    let tail = &review.agent_args[review.agent_args.len() - 2..];
    assert_eq!(tail, ["--tools", "read,grep,find,ls,bash"]);
    assert!(resolve_launch(&kind("pi", "m", Some("auto")), "tieude").is_err());
}

#[test]
fn test_launch_review_args_per_harness() {
    let cmd = |harness: &str, model: &str, effort: &str, review: bool| {
        let l = resolve_launch(&kind(harness, model, Some(effort)), "k").unwrap();
        build_launch_cmd(&l, "rev", "/r/x.rules.md", "/w/runs", "/repo/.wt/rev", review).unwrap()
    };

    // omp: review drops edit/write, keeps read tools + bash.
    let omp = cmd("omp", "google-antigravity/gemini-3.8-flash", "high", true);
    assert_eq!(
        omp.agent_args,
        vec![
            "--", "--model", "google-antigravity/gemini-3.8-flash", "--thinking", "high",
            "--auto-approve", "--tools=read,bash,grep,glob", "--append-system-prompt",
            "/r/x.rules.md",
        ]
    );
    let omp_build = cmd("omp", "google-antigravity/gemini-3.8-flash", "high", false);
    assert!(!omp_build.agent_args.iter().any(|a| a.starts_with("--tools")));

    // claude: today's settings, unchanged.
    let cl = cmd("claude", "opus-5.5", "high", true);
    assert_eq!(
        cl.agent_args,
        vec![
            "--", "--model", "claude-opus-5-5", "--effort", "high",
            "--permission-mode", "dontAsk", "--add-dir", "/w/runs", "--settings",
            r#"{"permissions":{"allow":["Edit(//w/runs/**)","Bash(cargo test:*)","Bash(mv *runs/.*.tmp *runs/*.md)"],"deny":["Edit(//repo/.wt/rev/**)"]}}"#,
            "--append-system-prompt-file", "/r/x.rules.md",
        ]
    );

    // opencode: no confirmed edit-deny config key yet — review launch equals
    // build launch; detection (`worker::prompt::readonly`) is the guard.
    let oc = cmd("opencode", "opencode-go/deepseek-v4.1-flash", "max", true);
    assert_eq!(oc, cmd("opencode", "opencode-go/deepseek-v4.1-flash", "max", false));
}

#[test]
fn test_launch_fails_closed_without_model_or_effort() {
    let e = resolve_launch(&kind("opencode", "m", None), "tieude")
        .unwrap_err()
        .1;
    assert!(e.contains("no effort in team.toml"), "{e}");

    let e = resolve_launch(&kind("opencode", "m", Some("banana")), "tieude")
        .unwrap_err()
        .1;
    assert!(e.contains("does not take effort 'banana'"), "{e}");

    // opencode model variant vocabulary is enforced per-model
    let e = resolve_launch(
        &kind(
            "opencode",
            "opencode-go/deepseek-v4.1-flash",
            Some("medium"),
        ),
        "tieude",
    )
    .unwrap_err()
    .1;
    assert!(e.contains("does not take effort 'medium'"), "{e}");

    // openrouter model variant vocabulary is matched on last segment
    let e = resolve_launch(
        &kind(
            "opencode",
            "openrouter/deepseek/deepseek-v4.1-flash",
            Some("medium"),
        ),
        "tieude",
    )
    .unwrap_err()
    .1;
    assert!(e.contains("does not take effort 'medium'"), "{e}");

    // unknown opencode model rejected with allowlist error
    let e = resolve_launch(
        &kind("opencode", "opencode-go/unknown-model", Some("max")),
        "tieude",
    )
    .unwrap_err()
    .1;
    assert!(e.contains("fix: add model and variants to team.toml [opencode_models]"), "{e}");

    // claude's vocabulary is narrower than opencode's
    let e = resolve_launch(&kind("claude", "m", Some("minimal")), "ni")
        .unwrap_err()
        .1;
    assert!(e.contains("does not take effort 'minimal'"), "{e}");

    let e = resolve_launch(&kind("opencode", "  ", Some("max")), "tieude")
        .unwrap_err()
        .1;
    assert!(e.contains("has no model"), "{e}");

    let e = resolve_launch(&kind("weird", "m", Some("max")), "x")
        .unwrap_err()
        .1;
    assert!(e.contains("unsupported harness"), "{e}");
}
#[test]
fn test_resolve_launch_uses_passed_cfg_for_opencode_models() {
    let mut cfg = config::Config::default();
    cfg.opencode_models.insert(
        "my-custom-model".to_string(),
        vec!["max".to_string()],
    );
    // Custom model passes because passed cfg has it in opencode_models
    let k = kind("opencode", "my-custom-model", Some("max"));
    let res = quaqua::launch::resolve_launch(&k, "custom-kind", &cfg);
    assert!(res.is_ok(), "expected custom model to launch: {:?}", res.err());

    // Without that entry in cfg, fails with hint naming team.toml
    let default_cfg = config::Config::default();
    let err = quaqua::launch::resolve_launch(&k, "custom-kind", &default_cfg).unwrap_err().1;
    assert!(
        err.contains("fix: add model and variants to team.toml [opencode_models]"),
        "got: {err}"
    );
}

#[test]
fn test_start_failure_reason_reads_herdr_error_code() {
    fn out(stdout: &str) -> Output {
        Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(1 << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }
    assert_eq!(
        start_failure_reason(&out(
            r#"{"error":{"code":"agent_not_ready","message":"blocked during startup"}}"#
        )),
        "agent_not_ready"
    );
    assert_eq!(
        start_failure_reason(&out(r#"{"error":{"code":"quota_exceeded"}}"#)),
        "quota_exceeded"
    );
    assert_eq!(
        start_failure_reason(&out("plain text, not json")),
        "start-failed"
    );
}

#[test]
fn test_fallback_kind_resolution() {
    let mut cfg = config::Config::default();
    cfg.kinds.insert(
        "tieude".to_string(),
        config::Kind {
            harness: "opencode".to_string(),
            model: "m".to_string(),
            effort: Some("max".to_string()),
            fallback: Some("detu".to_string()),
            can_review: false,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );
    cfg.kinds.insert(
        "detu".to_string(),
        config::Kind {
            harness: "opencode".to_string(),
            model: "m".to_string(),
            effort: Some("max".to_string()),
            fallback: None,
            can_review: false,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );

    // Valid fallback resolves to detu
    let (fb_name, fb_kind) = fallback_kind(&cfg, &cfg.kinds["tieude"], "tieude")
        .unwrap()
        .unwrap();
    assert_eq!(fb_name, "detu");
    assert_eq!(fb_kind.harness, "opencode");

    // No fallback returns Ok(None)
    assert!(fallback_kind(&cfg, &cfg.kinds["detu"], "detu")
        .unwrap()
        .is_none());

    // Fallback naming a missing kind returns Err
    let mut bad_kind = cfg.kinds["tieude"].clone();
    bad_kind.fallback = Some("ghost".to_string());
    let err = fallback_kind(&cfg, &bad_kind, "tieude").unwrap_err().1;
    assert!(err.contains("fallback 'ghost' is not a kind"), "{err}");
}

struct SpawnTestGit;
impl GitOps for SpawnTestGit {
    fn run_in(&self, _dir: &Path, _args: &[&str]) -> std::io::Result<Output> {
        Ok(Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
    fn run(&self, _args: &[&str]) -> std::io::Result<Output> {
        self.run_in(Path::new("."), _args)
    }
    fn rev_parse_commit(&self, _repo: &Path, _base: &str) -> bool {
        true
    }
    fn worktree_list_porcelain(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
        Ok(vec![])
    }
    fn worktree_add(
        &self,
        _repo: &Path,
        _branch: &str,
        wt: &Path,
        _base: Option<&str>,
    ) -> std::io::Result<Output> {
        let _ = std::fs::create_dir_all(wt);
        self.run_in(_repo, &[])
    }
    fn branch_exists(&self, _repo: &Path, _branch: &str) -> bool {
        false
    }
    fn worktree_add_existing(
        &self,
        _repo: &Path,
        _branch: &str,
        _wt: &Path,
    ) -> std::io::Result<Output> {
        self.run_in(_repo, &[])
    }
    fn worktree_remove(&self, _repo: &Path, wt: &Path) -> std::io::Result<Output> {
        let _ = std::fs::remove_dir_all(wt);
        self.run_in(_repo, &[])
    }
    fn remote_list(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
        Ok(vec![])
    }
    fn is_core_bare(&self, _repo: &Path) -> bool {
        false
    }
    fn has_core_worktree(&self, _repo: &Path) -> bool {
        false
    }
    fn enable_worktree_config(&self, _repo: &Path) -> std::io::Result<Output> {
        self.run_in(_repo, &[])
    }
    fn set_worktree_pushurl_blocked(&self, _wt: &Path, _remote: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn is_git_dir(&self, _cwd: &Path) -> bool {
        true
    }
    fn diff_and_status(&self, _cwd: &Path) -> (String, String) {
        ("dh".to_string(), "clean".to_string())
    }
}

fn test_cfg_with_fallback() -> config::Config {
    let mut cfg = config::Config::default();
    cfg.kinds.insert(
        "sude".to_string(),
        config::Kind {
            harness: "omp".to_string(),
            model: "gemini".to_string(),
            effort: Some("high".to_string()),
            fallback: Some("detu".to_string()),
            can_review: false,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );
    cfg.kinds.insert(
        "detu".to_string(),
        config::Kind {
            harness: "opencode".to_string(),
            model: "deepseek-v4.1-flash".to_string(),
            effort: Some("max".to_string()),
            fallback: Some("tieude".to_string()),
            can_review: false,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );
    cfg.kinds.insert(
        "ni".to_string(),
        config::Kind {
            harness: "claude".to_string(),
            model: "opus".to_string(),
            effort: Some("low".to_string()),
            fallback: None,
            can_review: true,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );
    cfg
}

struct EnvLockGuard;
static ENV_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn lock_env() -> EnvLockGuard {
    while ENV_BUSY
        .compare_exchange_weak(
            false,
            true,
            std::sync::atomic::Ordering::Acquire,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        std::thread::yield_now();
    }
    EnvLockGuard
}
impl Drop for EnvLockGuard {
    fn drop(&mut self) {
        std::env::remove_var("QUAQUA_RUNS_DIR");
        std::env::remove_var("QUAQUA_OWNER");
        std::env::remove_var("GRID_TAB");
        std::env::remove_var("GRID_CHECK_TIMEOUT_SEC");
        std::env::remove_var("HERDR_STUB_SNAPSHOT");
        std::env::remove_var("HERDR_PANE_ID");
        std::env::remove_var("GRID_SKIP");
        std::env::remove_var("QUAQUA_TEST");
        std::env::remove_var("QUAQUA_TEAM_TOML");
        std::env::remove_var("QUAQUA_HOME");
        ENV_BUSY.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[test]
fn test_spawn_worker_resolving_purpose() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-spawn-purpose-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("quaqua/runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    let mut cfg = test_cfg_with_fallback();
    cfg.kinds.insert(
        "suhuynh".to_string(),
        config::Kind {
            harness: "claude".to_string(),
            model: "opus".to_string(),
            effort: Some("low".to_string()),
            fallback: None,
            can_review: true,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );
    cfg.purposes.insert("review".to_string(), "suhuynh".to_string());

    let git = SpawnTestGit;
    let mut mock = MockActivities::new();
    mock.register(
        "herdr.pane_split",
        ActivityResult::Herdr(HerdrResult::AgentOutput {
            content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
        }),
    );
    mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
        name: String::new(),
    }));
    let herdr = MockActivitiesHerdr::new(mock);

    let opts = parse_spawn_args(&[
        "--purpose".to_string(),
        "review".to_string(),
        "suhuynh-purptest".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();
    assert_eq!(opts.purpose.as_deref(), Some("review"));
    assert_eq!(opts.kind, None);
    let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
    assert!(res.is_ok(), "spawn should resolve purpose and succeed: {:?}", res.err());

    let starts = herdr.agent_starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].0, "suhuynh-purptest");
    assert_eq!(starts[0].1, "claude");

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_spawn_worker_purpose_with_matching_positional_kind() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-spawn-purpose-match-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("quaqua/runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    let mut cfg = test_cfg_with_fallback();
    cfg.kinds.insert(
        "suhuynh".to_string(),
        config::Kind {
            harness: "claude".to_string(),
            model: "opus".to_string(),
            effort: Some("low".to_string()),
            fallback: None,
            can_review: true,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );
    cfg.purposes.insert("review".to_string(), "suhuynh".to_string());

    let git = SpawnTestGit;
    let mut mock = MockActivities::new();
    mock.register(
        "herdr.pane_split",
        ActivityResult::Herdr(HerdrResult::AgentOutput {
            content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
        }),
    );
    mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
        name: String::new(),
    }));
    let herdr = MockActivitiesHerdr::new(mock);

    let opts = parse_spawn_args(&[
        "--purpose".to_string(),
        "review".to_string(),
        "suhuynh-purpmatch".to_string(),
        "suhuynh".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();
    let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
    assert!(res.is_ok(), "spawn with matching kind should succeed: {:?}", res.err());
    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let spawned: Vec<_> = events.iter().filter(|e| e.event_type == "spawned").collect();
    assert_eq!(spawned.len(), 1, "must record exactly one spawned event");
    assert_eq!(spawned[0].subject.as_deref(), Some("suhuynh-purpmatch"));


    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_spawn_worker_purpose_with_mismatched_positional_kind_fails() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-spawn-purpose-mismatch-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("quaqua/runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    let mut cfg = test_cfg_with_fallback();
    cfg.kinds.insert(
        "suhuynh".to_string(),
        config::Kind {
            harness: "claude".to_string(),
            model: "opus".to_string(),
            effort: Some("low".to_string()),
            fallback: None,
            can_review: true,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );
    cfg.purposes.insert("review".to_string(), "suhuynh".to_string());

    let git = SpawnTestGit;
    let herdr = MockActivitiesHerdr::new(MockActivities::new());

    let opts = parse_spawn_args(&[
        "--purpose".to_string(),
        "review".to_string(),
        "suhuynh-purpmismatch".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();
    let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
    assert!(res.is_err(), "spawn with mismatched kind should fail");
    let (code, msg) = res.unwrap_err();
    assert_eq!(code, 1);
    assert!(
        msg.contains("suhuynh") && msg.contains("sude"),
        "error message should name both resolved kind 'suhuynh' and positional kind 'sude', got: {msg}"
    );
    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "spawn_failed");
    assert_eq!(events[0].subject.as_deref(), Some("suhuynh-purpmismatch"));


    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_spawn_worker_purpose_with_colon_resolves_correctly() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-spawn-purpose-colon-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("quaqua/runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    let mut cfg = test_cfg_with_fallback();
    cfg.kinds.insert(
        "sude".to_string(),
        config::Kind {
            harness: "omp".to_string(),
            model: "gemini".to_string(),
            effort: Some("high".to_string()),
            fallback: None,
            can_review: false,
            spawn: config::SPAWN_AUTO.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );
    cfg.purposes.insert("fix:ui".to_string(), "sude".to_string());

    let git = SpawnTestGit;
    let mut mock = MockActivities::new();
    mock.register(
        "herdr.pane_split",
        ActivityResult::Herdr(HerdrResult::AgentOutput {
            content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
        }),
    );
    mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
        name: String::new(),
    }));
    let herdr = MockActivitiesHerdr::new(mock);

    let opts = parse_spawn_args(&[
        "--purpose".to_string(),
        "fix:ui".to_string(),
        "sude-colontest".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();
    let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
    assert!(res.is_ok(), "spawn with colon in purpose should resolve correctly: {:?}", res.err());

    let starts = herdr.agent_starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].0, "sude-colontest");

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_renamed_kind_passes_cap_cleanup_cost() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-renamed-kind-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("quaqua/runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);

    let team_path = temp.join("team.toml");
    let team_content = r#"schema_version = 2
[limits]
depth = 2
max_workers_per_orchestrator = 4
weights = { default = 1, pipeline = 2 }

[timers]
budget_default = "45m"
hard_cap = "120m"
stall = "10m"
idle = "2m"
nudge_at = 0.8
grace = 0.25
pilot_budget = "30m"

[strategy]
default_kind = "nhanvat"
default_reviewer = ["nhanvat"]
builder_reviewer_differ = "none"

[purposes]
work = "nhanvat"

[kinds.tieude]
harness = "opencode"
model = "opencode-go/deepseek-v4.1-flash"
effort = "max"

[kinds.sude]
harness = "omp"
model = "google-antigravity/gemini-3.8-flash"
effort = "high"

[kinds.suhuynh]
harness = "claude"
model = "opus-5.5"
effort = "high"
can_review = true

[kinds.nhanvat]
harness = "opencode"
model = "opencode-go/deepseek-v4.1-flash"
effort = "max"
can_review = true

[grid]
workers_tab = "workers"
"#;
    std::fs::write(&team_path, team_content).unwrap();

    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");
    std::env::set_var("QUAQUA_TEAM_TOML", team_path.to_str().unwrap());

    let cfg = quaqua::config::load_team_config().expect("loads custom team.toml");
    assert!(cfg.kinds.contains_key("nhanvat"));

    // 1. Cap: is_task_worker_name dynamically recognizes nhanvat from team.toml
    assert!(quaqua::cap::is_task_worker_name("nhanvat-test1", &cfg));
    assert!(!quaqua::cap::is_task_worker_name("nhanvat-lead", &cfg));
    assert!(!quaqua::cap::is_task_worker_name("unknown-test1", &cfg));

    // 2. Cost: guess_harness_from_config correctly maps nhanvat to opencode
    assert!(quaqua::worker::prompt::cost::is_opencode_worker("", "nhanvat-test1"));
    assert!(!quaqua::worker::prompt::cost::is_omp_worker("", "nhanvat-test1"));

    // 3. Cleanup: cleanup_worker correctly records telemetry with kind = "nhanvat"
    let worker_name = "nhanvat-test1";
    let wt = temp.join(".wt").join(worker_name);
    let _ = std::fs::create_dir_all(&wt);
    std::fs::write(runs_dir.join(format!("{worker_name}.owner")), "cos\n").unwrap();

    let herdr = MockActivitiesHerdr::for_cleanup("w1:p3");
    let git = SpawnTestGit;
    let res = quaqua::worker::cleanup::cleanup_worker(worker_name, &repo_dir, &git, &herdr);
    assert!(res.is_ok(), "cleanup failed: {:?}", res);

    // Verify telemetry JSONL has kind: "nhanvat"
    let tele_path = runs_dir.join("telemetry.jsonl");
    assert!(tele_path.exists());
    let content = std::fs::read_to_string(&tele_path).unwrap();
    assert!(content.contains("\"kind\":\"nhanvat\""), "telemetry missing kind 'nhanvat': {content}");

    let _ = std::fs::remove_dir_all(&temp);
}
#[test]
fn test_hyphenated_kind_resolution_ni_fast_vs_ni() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-hyphen-kind-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("quaqua/runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);

    let team_path = temp.join("team.toml");
    let team_content = r#"schema_version = 2
[limits]
depth = 2
max_workers_per_orchestrator = 4
weights = { default = 1, pipeline = 2 }

[timers]
budget_default = "45m"
hard_cap = "120m"
stall = "10m"
idle = "2m"
nudge_at = 0.8
grace = 0.25
pilot_budget = "30m"

[strategy]
default_kind = "ni"
default_reviewer = ["ni"]
builder_reviewer_differ = "none"

[kinds.ni]
harness = "claude"
model = "opus-5.5"
effort = "low"
can_review = true

[kinds.ni-fast]
harness = "opencode"
model = "opencode-go/deepseek-v4.1-flash"
effort = "max"
can_review = true

[grid]
workers_tab = "workers"
"#;
    std::fs::write(&team_path, team_content).unwrap();

    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");
    std::env::set_var("QUAQUA_TEAM_TOML", team_path.to_str().unwrap());

    let worker_name = "ni-fast-worker1";

    // 1. Cost harness guessing must resolve to opencode, not claude
    assert!(
        quaqua::worker::prompt::cost::is_opencode_worker("", worker_name),
        "ni-fast-worker1 must be recognized as opencode worker"
    );
    assert!(
        !quaqua::worker::prompt::cost::is_claude_worker("", worker_name),
        "ni-fast-worker1 must NOT be recognized as claude worker"
    );

    // 2. Cleanup telemetry must record kind = "ni-fast", not "ni"
    let wt = temp.join(".wt").join(worker_name);
    let _ = std::fs::create_dir_all(&wt);
    std::fs::write(runs_dir.join(format!("{worker_name}.owner")), "cos\n").unwrap();

    let herdr = MockActivitiesHerdr::for_cleanup("w1:p3");
    let git = SpawnTestGit;
    let res = quaqua::worker::cleanup::cleanup_worker(worker_name, &repo_dir, &git, &herdr);
    assert!(res.is_ok(), "cleanup failed: {:?}", res);

    let tele_path = runs_dir.join("telemetry.jsonl");
    assert!(tele_path.exists());
    let content = std::fs::read_to_string(&tele_path).unwrap();
    assert!(
        content.contains("\"kind\":\"ni-fast\""),
        "telemetry must record kind 'ni-fast', got: {content}"
    );
    assert!(
        content.contains("\"harness\":\"opencode\""),
        "telemetry must record harness 'opencode', got: {content}"
    );

    let _ = std::fs::remove_dir_all(&temp);
}
#[test]
fn test_spawn_worker_fallback_scenarios() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-spawn-fb-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("quaqua/runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    // Scenario 1: Primary start fails (omp), falls back to detu (opencode)
    {
        let cfg = test_cfg_with_fallback();
        let git = SpawnTestGit;
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.pane_split",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
            }),
        );
        mock.register(
            "herdr.agent_start:omp",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content:
                    r#"{"error":{"code":"agent_not_ready","message":"blocked during startup"}}"#
                        .to_string(),
            }),
        );
        mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
            name: String::new(),
        }));
        let herdr = MockActivitiesHerdr::new(mock);

        let opts = parse_spawn_args(&[
            "sude-fbtest".to_string(),
            "sude".to_string(),
            repo_dir.to_str().unwrap().to_string(),
        ])
        .unwrap();

        let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
        assert!(
            res.is_ok(),
            "spawn should succeed with fallback: {:?}",
            res.err()
        );

        let starts = herdr.agent_starts();
        assert_eq!(
            starts.len(),
            2,
            "expected exactly 2 starts (primary, fallback)"
        );
        assert_eq!(starts[0], ("sude-fbtest".to_string(), "omp".to_string()));
        assert_eq!(
            starts[1],
            ("sude-fbtest".to_string(), "opencode".to_string())
        );
    }

    // Scenario 2: One hop limit — both primary and fallback fail -> error after 2 starts
    {
        let cfg = test_cfg_with_fallback();
        let git = SpawnTestGit;
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.pane_split",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
            }),
        );
        mock.register(
            "herdr.agent_start:omp",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content:
                    r#"{"error":{"code":"agent_not_ready","message":"blocked during startup"}}"#
                        .to_string(),
            }),
        );
        mock.register(
            "herdr.agent_start:opencode",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content:
                    r#"{"error":{"code":"agent_not_ready","message":"blocked during startup"}}"#
                        .to_string(),
            }),
        );
        mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
            name: String::new(),
        }));
        let herdr = MockActivitiesHerdr::new(mock);

        let opts = parse_spawn_args(&[
            "sude-onehop".to_string(),
            "sude".to_string(),
            repo_dir.to_str().unwrap().to_string(),
        ])
        .unwrap();

        let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
        assert!(res.is_err(), "must error when fallback fails");

        let starts = herdr.agent_starts();
        assert_eq!(starts.len(), 2, "expected exactly 2 starts (one hop max)");
    }

    // Scenario 3: Fallback naming missing kind -> error
    {
        let mut cfg = test_cfg_with_fallback();
        cfg.kinds.get_mut("sude").unwrap().fallback = Some("ghost".to_string());
        let git = SpawnTestGit;
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.pane_split",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
            }),
        );
        mock.register(
            "herdr.agent_start:omp",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content:
                    r#"{"error":{"code":"agent_not_ready","message":"blocked during startup"}}"#
                        .to_string(),
            }),
        );
        mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
            name: String::new(),
        }));
        let herdr = MockActivitiesHerdr::new(mock);

        let opts = parse_spawn_args(&[
            "sude-fbmiss".to_string(),
            "sude".to_string(),
            repo_dir.to_str().unwrap().to_string(),
        ])
        .unwrap();

        let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
        assert!(res.is_err(), "must error when fallback kind is missing");

        let starts = herdr.agent_starts();
        assert_eq!(
            starts.len(),
            1,
            "only primary attempted before bad fallback error"
        );
    }

    // Scenario 4: A review spawn (--review) refuses fallback to a non-review kind
    {
        let mut cfg = test_cfg_with_fallback();
        cfg.kinds.get_mut("ni").unwrap().fallback = Some("detu".to_string());
        let git = SpawnTestGit;
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.pane_split",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
            }),
        );
        mock.register(
            "herdr.agent_start:claude",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content:
                    r#"{"error":{"code":"agent_not_ready","message":"blocked during startup"}}"#
                        .to_string(),
            }),
        );
        mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
            name: String::new(),
        }));
        let herdr = MockActivitiesHerdr::new(mock);

        let opts = parse_spawn_args(&[
            "--review".to_string(),
            "ni-revtest".to_string(),
            "ni".to_string(),
            repo_dir.to_str().unwrap().to_string(),
        ])
        .unwrap();

        let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
        assert!(
            res.is_err(),
            "must refuse fallback to non-review kind when in review mode"
        );
        let starts = herdr.agent_starts();
        assert_eq!(
            starts.len(),
            1,
            "must not attempt start of non-review fallback kind"
        );
    }

    std::env::remove_var("QUAQUA_RUNS_DIR");
    std::env::remove_var("QUAQUA_TEST");
    std::env::remove_var("GRID_SKIP");
    let _ = std::fs::remove_dir_all(&temp);
}

/// A kind with `spawn = "approve"` must refuse a plain spawn (red) and start
/// once `--approve-kind <kind>` is given (green); the flag must never leak to
/// other kinds.
#[test]
fn test_spawn_worker_approve_policy_gate() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-spawn-approve-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    let mut cfg = test_cfg_with_fallback();
    cfg.kinds.insert(
        "truonglao".to_string(),
        config::Kind {
            harness: "claude".to_string(),
            model: "opus".to_string(),
            effort: Some("high".to_string()),
            fallback: None,
            can_review: true,
            spawn: config::SPAWN_APPROVE.to_string(),
            on_cost_cap: config::COST_CAP_WARN.to_string(),
        },
    );

    let git = SpawnTestGit;
    let repo = repo_dir.to_str().unwrap().to_string();

    // Red: no flag -> refused before any pane/agent start.
    let herdr = MockActivitiesHerdr::for_spawn();
    let opts = parse_spawn_args(&[
        "truonglao-approvet".to_string(),
        "truonglao".to_string(),
        repo.clone(),
    ])
    .unwrap();
    let err = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli).unwrap_err();
    assert!(err.1.contains("--approve-kind truonglao"), "{}", err.1);
    assert!(
        !herdr.has_call("herdr.agent_start:claude"),
        "refused spawn must not start an agent"
    );

    // Green: the explicit flag parsed from the CLI approves exactly this kind.
    let herdr = MockActivitiesHerdr::for_spawn();
    let opts = parse_spawn_args(&[
        "--approve-kind".to_string(),
        "truonglao".to_string(),
        "truonglao-approvet".to_string(),
        "truonglao".to_string(),
        repo,
    ])
    .unwrap();
    assert_eq!(opts.approve_kinds, vec!["truonglao".to_string()]);
    quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli)
        .expect("approved spawn should start");
    assert!(herdr.has_call("herdr.agent_start:claude"));

    std::env::remove_var("QUAQUA_RUNS_DIR");
    std::env::remove_var("QUAQUA_TEST");
    std::env::remove_var("GRID_SKIP");
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_spawn_worker_default_grid_tab_from_owner() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-spawn-tab-{}", std::process::id()));
    // The repo dir name is deliberately NOT the project slug: a Lead's project
    // (`quaqua-rs`) spawns into the `quaqua` checkout.
    let repo_dir = temp.join("quaqua");
    let runs_dir = temp.join("quaqua/runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");
    std::env::remove_var("GRID_TAB");

    let cfg = test_cfg_with_fallback();
    let git = SpawnTestGit;
    let herdr = MockActivitiesHerdr::for_spawn();
    let spawn_and_read_tab = |name: &str| -> String {
        let opts = parse_spawn_args(&[
            name.to_string(),
            "sude".to_string(),
            repo_dir.to_str().unwrap().to_string(),
        ])
        .unwrap();
        let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
        assert!(res.is_ok(), "spawn {name} failed: {:?}", res.err());
        std::fs::read_to_string(runs_dir.join(format!("{name}.grid")))
            .expect("runs/<name>.grid must be recorded")
            .trim()
            .to_string()
    };

    // A Lead-owned worker gets the project tab from the owner (`<slug>-lead` ->
    // `<slug>-workers`), never a tab derived from the repo dir name — otherwise
    // siblings of the same owner split across `quaqua-workers` and
    // `quaqua-rs-workers` (runs/tieude-testlow.grid vs runs/tieude-spawneff.grid).
    std::env::set_var("QUAQUA_OWNER", "quaqua-rs-lead");
    assert_eq!(spawn_and_read_tab("sude-tablead"), "quaqua-rs-workers");

    // The CoS owner is not a project slug: it keeps the repo-derived default.
    std::env::set_var("QUAQUA_OWNER", "cos");
    assert_eq!(spawn_and_read_tab("sude-tabcos"), "quaqua-workers");

    // An explicit GRID_TAB still wins over the owner-derived default.
    std::env::set_var("QUAQUA_OWNER", "quaqua-rs-lead");
    std::env::set_var("GRID_TAB", "quaqua-rs-workers-2");
    assert_eq!(spawn_and_read_tab("sude-tabexp"), "quaqua-rs-workers-2");
    std::env::remove_var("GRID_TAB");

    std::env::remove_var("QUAQUA_RUNS_DIR");
    std::env::remove_var("QUAQUA_OWNER");
    std::env::remove_var("GRID_SKIP");
    std::env::remove_var("QUAQUA_TEST");
    let _ = std::fs::remove_dir_all(&temp);
}

/// Spawn `name` against a fake herdr whose `pane get` reports `pane_json` for
/// the caller pane, and return the recorded `(owner, grid tab)`.
fn spawn_with_caller_pane(
    name: &str,
    repo_dir: &Path,
    runs_dir: &Path,
    cfg: &config::Config,
    pane_json: &str,
) -> (String, String) {
    let mut mock = MockActivities::new();
    mock.register(
        "herdr.pane_split",
        ActivityResult::Herdr(HerdrResult::AgentOutput {
            content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
        }),
    );
    mock.register(
        "herdr.run",
        ActivityResult::Herdr(HerdrResult::AgentOutput {
            content: pane_json.to_string(),
        }),
    );
    mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
        name: String::new(),
    }));
    let herdr = MockActivitiesHerdr::new(mock);
    let opts = parse_spawn_args(&[
        name.to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();
    let res = quaqua::worker::spawn::spawn_worker(opts, cfg, &SpawnTestGit, &herdr, quaqua::layout::SpawnOrigin::Cli);
    assert!(res.is_ok(), "spawn {name} failed: {:?}", res.err());
    let owner = std::fs::read_to_string(runs_dir.join(format!("{name}.owner"))).unwrap();
    let grid = std::fs::read_to_string(runs_dir.join(format!("{name}.grid"))).unwrap();
    (owner.trim().to_string(), grid.trim().to_string())
}

#[test]
fn test_spawn_owner_from_caller_pane_not_stale_env() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-spawn-paneown-{}", std::process::id()));
    let repo_dir = temp.join("quaqua");
    let runs_dir = temp.join("quaqua/runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");
    std::env::remove_var("GRID_TAB");
    std::env::set_var("HERDR_PANE_ID", "wA:p1");

    let cfg = test_cfg_with_fallback();
    let lead_pane = r#"{"result":{"pane":{"pane_id":"wA:p1","tokens":{"grid_role":"orchestrator","grid_owner":"arch-lead"}}}}"#;

    // A Lead's shell can still say `cos` (a herdr restart restores the daemon's
    // environment); its pane metadata is the durable record of who it is, and
    // the worker must land in the project's tab, not the CoS repo tab.
    std::env::set_var("QUAQUA_OWNER", "cos");
    let (owner, grid) =
        spawn_with_caller_pane("sude-panelead", &repo_dir, &runs_dir, &cfg, lead_pane);
    assert_eq!(owner, "arch-lead", "pane tag must beat the stale shell env");
    assert_eq!(grid, "arch-workers");

    // An explicit project owner in the environment stays authoritative: a
    // pipeline spawns its Lead's workers with QUAQUA_OWNER=<slug>-lead from
    // whatever pane it happens to run in.
    std::env::set_var("QUAQUA_OWNER", "quaqua-rs-lead");
    let (owner, grid) =
        spawn_with_caller_pane("sude-envlead", &repo_dir, &runs_dir, &cfg, lead_pane);
    assert_eq!(owner, "quaqua-rs-lead");
    assert_eq!(grid, "quaqua-rs-workers");

    // A worker pane never overrides the environment.
    std::env::set_var("QUAQUA_OWNER", "cos");
    let worker_pane = r#"{"result":{"pane":{"pane_id":"wA:p9","tokens":{"grid_role":"worker","grid_owner":"arch-lead"}}}}"#;
    let (owner, grid) =
        spawn_with_caller_pane("sude-paneworker", &repo_dir, &runs_dir, &cfg, worker_pane);
    assert_eq!(owner, "cos");
    assert_eq!(grid, "quaqua-workers");

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_default_grid_tab_helper() {
    use quaqua::worker::spawn::default_grid_tab;

    // Lead owners (<slug>-lead) resolve to <slug>-workers
    assert_eq!(
        default_grid_tab("quaqua-grid-lead", "myrepo"),
        "quaqua-grid-workers"
    );
    assert_eq!(
        default_grid_tab("quaqua-rs-lead", "quaqua"),
        "quaqua-rs-workers"
    );

    // Non-lead owners (cos, arbitrary names) resolve to <repo>-workers
    assert_eq!(default_grid_tab("cos", "myrepo"), "myrepo-workers");
    assert_eq!(default_grid_tab("", "myrepo"), "myrepo-workers");
    assert_eq!(default_grid_tab("other", "myrepo"), "myrepo-workers");

    // Edge cases: -lead with empty slug falls back to <repo>-workers
    assert_eq!(default_grid_tab("-lead", "myrepo"), "myrepo-workers");
    assert_eq!(default_grid_tab("lead", "myrepo"), "myrepo-workers");
}

#[test]
fn test_spawn_worker_grid_timeout_fails() {
    let temp = std::env::temp_dir().join(format!("quaqua-spawn-gridfail-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let _env_lock = lock_env();
    let runs_dir = temp.join("quaqua/runs");
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_OWNER", "cos");
    std::env::set_var("GRID_TAB", "quaqua-workers");
    std::env::set_var("GRID_CHECK_TIMEOUT_SEC", "0");
    std::env::remove_var("GRID_SKIP");

    std::env::set_var("HERDR_BIN_PATH", "/dev/null");

    let cfg = test_cfg_with_fallback();
    let git = SpawnTestGit;
    let herdr = MockActivitiesHerdr::for_spawn();

    let opts = parse_spawn_args(&[
        "sude-gridfail".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);

    std::env::remove_var("QUAQUA_RUNS_DIR");
    std::env::remove_var("QUAQUA_OWNER");
    std::env::remove_var("GRID_TAB");
    std::env::remove_var("GRID_CHECK_TIMEOUT_SEC");
    std::env::remove_var("HERDR_BIN_PATH");
    let _ = std::fs::remove_dir_all(&temp);

    let err = res.expect_err("spawn_worker must fail when grid check times out");
    assert_eq!(err.0, 1);
    assert!(err.1.contains("grid"), "error must mention grid: {}", err.1);
    assert!(
        err.1.contains("sude-gridfail"),
        "error must name the live worker: {}",
        err.1
    );
}

#[test]
fn test_cleanup_worker_grid_timeout_fails() {
    let temp = std::env::temp_dir().join(format!("quaqua-clean-gridfail-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("quaqua/runs");
    let _env_lock = lock_env();
    let _ = std::fs::create_dir_all(&repo_dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    let wt = temp.join(".wt/sude-gridfail");
    let _ = std::fs::create_dir_all(&wt);
    let owner_file = runs_dir.join("sude-gridfail.owner");
    let _ = std::fs::write(&owner_file, "cos\n");
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_OWNER", "cos");
    std::env::set_var("GRID_TAB", "quaqua-workers");
    std::env::set_var("GRID_CHECK_TIMEOUT_SEC", "0");
    std::env::remove_var("GRID_SKIP");

    std::env::set_var("HERDR_BIN_PATH", "/dev/null");

    let git = SpawnTestGit;
    let herdr = MockActivitiesHerdr::for_cleanup("w1:p2");

    let res = quaqua::worker::cleanup::cleanup_worker("sude-gridfail", &repo_dir, &git, &herdr);
    assert_eq!(
        herdr
            .calls()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["herdr.agent_get", "herdr.run", "herdr.pane_close"]
    );

    let wt_exists = wt.exists();
    std::env::remove_var("QUAQUA_RUNS_DIR");
    std::env::remove_var("QUAQUA_OWNER");
    std::env::remove_var("GRID_TAB");
    std::env::remove_var("GRID_CHECK_TIMEOUT_SEC");
    std::env::remove_var("HERDR_BIN_PATH");
    let _ = std::fs::remove_dir_all(&temp);

    let err = res.expect_err("cleanup_worker must fail when grid check times out");
    assert_eq!(err.0, 1);
    assert!(err.1.contains("grid"), "error must mention grid: {}", err.1);
    assert!(
        !wt_exists,
        "worktree must be removed even when grid check fails"
    );
}

// --- quaqua worker stop ---------------------------------------------------

use quaqua::worker::stop::{harness_from_info, parse_stop_args, stop_keys, stop_worker};

fn agent_get_json(harness: &str) -> String {
    format!(
        "{{\"result\":{{\"agent\":{{\"agent\":\"{harness}\",\"name\":\"tieude-x\",\"pane_id\":\"w1:p2\"}}}}}}"
    )
}

#[test]
fn test_stop_keys_by_harness() {
    assert_eq!(stop_keys("opencode"), Some(&["esc", "esc"][..]));
    assert_eq!(stop_keys("omp"), Some(&["esc"][..]));
    // No verified sequence for claude yet (Lead decision B, 2026-10-17):
    // esc esc would open Claude Code's message selector, so stop refuses.
    assert_eq!(stop_keys("claude"), None);
    assert_eq!(stop_keys("aider"), None);
    assert_eq!(stop_keys(""), None);
}

#[test]
fn test_stop_opencode_sends_esc_twice_in_order() {
    let herdr = MockActivitiesHerdr::with_agent_get(&agent_get_json("opencode"));
    stop_worker("tieude-x", &herdr).unwrap();
    assert_eq!(
        herdr.send_keys_calls(),
        vec![
            ("tieude-x".to_string(), "esc".to_string()),
            ("tieude-x".to_string(), "esc".to_string()),
        ]
    );
}

#[test]
fn test_stop_omp_sends_esc_once() {
    let herdr = MockActivitiesHerdr::with_agent_get(&agent_get_json("omp"));
    stop_worker("sude-x", &herdr).unwrap();
    assert_eq!(
        herdr.send_keys_calls(),
        vec![("sude-x".to_string(), "esc".to_string())]
    );
}

#[test]
fn test_stop_unmapped_harness_fails_closed_and_sends_nothing() {
    for harness in ["claude", "aider"] {
        let herdr = MockActivitiesHerdr::with_agent_get(&agent_get_json(harness));
        let err = stop_worker("tieude-x", &herdr).unwrap_err();
        assert_eq!(err.0, 1, "exit code for {harness}");
        assert!(
            err.1.contains(harness),
            "error names harness {harness}: {}",
            err.1
        );
        assert!(
            err.1.contains("tieude-x"),
            "error names the worker: {}",
            err.1
        );
        assert!(herdr.send_keys_calls().is_empty(), "no keys for {harness}");
    }
}

#[test]
fn test_stop_missing_agent_fails_closed_and_sends_nothing() {
    let herdr = MockActivitiesHerdr::with_agent_get(
        r#"{"error":{"code":"agent_not_found","message":"no such agent"}}"#,
    );
    let err = stop_worker("tieude-gone", &herdr).unwrap_err();
    assert_eq!(err.0, 1);
    assert!(err.1.contains("tieude-gone"), "{}", err.1);
    assert!(herdr.send_keys_calls().is_empty());
}

#[test]
fn test_stop_harness_from_info_parses_only_a_real_agent() {
    assert_eq!(
        harness_from_info(&agent_get_json("opencode")).as_deref(),
        Some("opencode")
    );
    assert_eq!(
        harness_from_info(&agent_get_json("omp")).as_deref(),
        Some("omp")
    );
    // Agent gone, harness missing/empty, or garbage: no harness, nothing to send.
    assert_eq!(
        harness_from_info(r#"{"error":{"code":"agent_not_found"}}"#),
        None
    );
    assert_eq!(
        harness_from_info(r#"{"result":{"agent":{"agent":""}}}"#),
        None
    );
    assert_eq!(harness_from_info(r#"{"result":{}}"#), None);
    assert_eq!(harness_from_info("not json"), None);
}

#[test]
fn test_stop_unreachable_herdr_is_reported_as_io_not_missing_agent() {
    let herdr = MockActivitiesHerdr::with_unreachable("No such file or directory (os error 2)");
    let err = stop_worker("tieude-x", &herdr).unwrap_err();
    assert_eq!(err.0, 1);
    assert!(err.1.contains("cannot reach herdr"), "{}", err.1);
    assert!(err.1.contains("No such file or directory"), "{}", err.1);
    assert!(
        !err.1.contains("not running"),
        "herdr I/O is not a missing agent: {}",
        err.1
    );
    assert!(herdr.send_keys_calls().is_empty());
}

#[test]
fn test_stop_requires_exactly_one_valid_name() {
    assert!(parse_stop_args(&[]).is_err());
    assert!(parse_stop_args(&["".to_string()]).is_err());
    assert!(parse_stop_args(&["a".to_string(), "b".to_string()]).is_err());
    assert!(parse_stop_args(&["Tieude-X".to_string()]).is_err());
    assert!(parse_stop_args(&["--force".to_string()]).is_err());
    assert_eq!(
        parse_stop_args(&["tieude-x".to_string()]).unwrap(),
        "tieude-x"
    );
}

#[test]
fn test_cleanup_worker_removes_per_worker_opencode_state() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-clean-state-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("quaqua/runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();

    let worker_name = "tieude-cleanup";
    let wt = temp.join(".wt").join(worker_name);
    std::fs::create_dir_all(&wt).unwrap();

    let state_dir = runs_dir.join(".opencode-state").join(worker_name);
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("model.json"), "{}").unwrap();
    assert!(state_dir.exists());

    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");
    let herdr = MockActivitiesHerdr::for_cleanup("w1:p2");
    let git = SpawnTestGit;
    let res = quaqua::worker::cleanup::cleanup_worker(worker_name, &repo_dir, &git, &herdr);
    assert!(res.is_ok(), "cleanup_worker failed: {:?}", res);
    assert_eq!(
        herdr
            .calls()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["herdr.agent_get", "herdr.run", "herdr.pane_close"]
    );
    assert!(
        !state_dir.exists(),
        "per-worker state dir must be removed on cleanup"
    );

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_update_index_content_markdown_table() {
    let original = "# Projects\n\n\
        | slug | repo | owner | goal | status |\n\
        |---|---|---|---|---|\n\
        | mono-desk | ~/Lab/mono-desk | cos | Port zdesk UI | active |\n\
        | baroiplayer | ~/Personal/baroiplayer | baroiplayer-lead | Publish lib | active |\n\
        | zimage | ~/Personal/zimage | cos | ImgMole | active |\n\n\
        Card: `projects/<slug>.md`.\n";

    // Update mono-desk owner to mono-desk-lead
    let updated = update_index_content(original, "mono-desk", "mono-desk-lead");
    assert!(
        updated
            .contains("| mono-desk | ~/Lab/mono-desk | mono-desk-lead | Port zdesk UI | active |"),
        "mono-desk owner cell not updated: {updated}"
    );
    assert!(
        updated.contains(
            "| baroiplayer | ~/Personal/baroiplayer | baroiplayer-lead | Publish lib | active |"
        ),
        "other rows must be preserved: {updated}"
    );
    assert!(
        updated.contains("| zimage | ~/Personal/zimage | cos | ImgMole | active |"),
        "other rows must be preserved: {updated}"
    );
    assert!(
        updated.contains("| slug | repo | owner | goal | status |"),
        "header row must remain unchanged: {updated}"
    );

    // Reset back to cos: should match original byte-for-byte
    let restored = update_index_content(&updated, "mono-desk", "cos");
    assert_eq!(restored, original);

    // Updating non-existent slug returns byte-identical content
    let untouched = update_index_content(original, "unknown-slug", "random-lead");
    assert_eq!(untouched, original);

    // Rows with fewer than 4 fields are preserved as-is without crashing or modifying
    let malformed = "| short | row\n";
    assert_eq!(
        update_index_content(malformed, "short", "lead"),
        "| short | row\n"
    );
}

#[test]
fn test_update_project_index_owner_fixture_atomic() {
    let temp = std::env::temp_dir().join(format!("quaqua-test-index-{}", std::process::id()));
    let projects_dir = temp.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    let index_file = projects_dir.join("INDEX.md");

    let initial_content = "# Projects\n\n\
        | slug | repo | owner | goal | status |\n\
        |---|---|---|---|---|\n\
        | baro | ~/Lab/baro | cos | player | active |\n\
        | baro-web | ~/Lab/baro-web | cos | site | active |\n";
    std::fs::write(&index_file, initial_content).unwrap();

    // 1. Update baro to baro-lead
    let res = update_project_index_owner(&temp, "baro", "baro-lead");
    assert!(res.is_ok(), "update_project_index_owner failed: {:?}", res);

    let read_back = std::fs::read_to_string(&index_file).unwrap();
    assert!(read_back.contains("| baro | ~/Lab/baro | baro-lead | player | active |"));
    assert!(read_back.contains("| baro-web | ~/Lab/baro-web | cos | site | active |"));

    // Lock and tmp files must be cleaned up
    assert!(!temp.join("projects/INDEX.md.lock.d").exists());
    for entry in std::fs::read_dir(&projects_dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !name.starts_with("INDEX.md.tmp."),
            "leftover tmp file: {name}"
        );
    }

    // 2. Reset baro to cos
    let res2 = update_project_index_owner(&temp, "baro", "cos");
    assert!(res2.is_ok());
    let read_back2 = std::fs::read_to_string(&index_file).unwrap();
    assert_eq!(read_back2, initial_content);

    // 3. Missing index file returns error
    let missing_temp = temp.join("nonexistent");
    let res_err = update_project_index_owner(&missing_temp, "baro", "cos");
    assert!(res_err.is_err());
}

#[test]
fn test_cleanup_worker_cleans_budget_grace_cost_and_metadata() {
    let _env_lock = lock_env();
    let temp = std::env::temp_dir().join(format!("quaqua-clean-all-{}", std::process::id()));
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("quaqua/runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();

    let worker_name = "sude-cleanall";
    let wt = temp.join(".wt").join(worker_name);
    std::fs::create_dir_all(&wt).unwrap();

    let files_to_create = [
        runs_dir.join(format!("{worker_name}.owner")),
        runs_dir.join(format!("{worker_name}.grid")),
        runs_dir.join(format!("{worker_name}.pipeline")),
        runs_dir.join(format!(".{worker_name}.provider_retry")),
        runs_dir.join(format!(".{worker_name}.cost80")),
        runs_dir.join(format!(".{worker_name}.cost100")),
        runs_dir.join(format!(".{worker_name}.progress")),
        runs_dir.join(format!(".{worker_name}.elapsed")),
        runs_dir.join(format!(".{worker_name}.nudged")),
        runs_dir.join(format!(".{worker_name}.submit.log")),
    ];

    for f in &files_to_create {
        std::fs::write(f, "test").unwrap();
        assert!(f.exists());
    }

    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");

    let herdr = MockActivitiesHerdr::for_cleanup("w1:p3");
    let git = SpawnTestGit;
    let res = quaqua::worker::cleanup::cleanup_worker(worker_name, &repo_dir, &git, &herdr);
    assert!(res.is_ok(), "cleanup_worker failed: {:?}", res);
    assert_eq!(
        herdr
            .calls()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["herdr.agent_get", "herdr.run", "herdr.pane_close"]
    );

    for f in &files_to_create {
        assert!(!f.exists(), "file must be cleaned up: {}", f.display());
    }

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_update_project_index_owner_concurrency() {
    use std::sync::Arc;
    use std::thread;

    let temp = Arc::new(
        std::env::temp_dir().join(format!("quaqua-test-index-race-{}", std::process::id())),
    );
    let projects_dir = temp.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    let index_file = projects_dir.join("INDEX.md");

    let mut initial = String::from("# Projects\n\n| slug | repo | owner |\n|---|---|---|\n");
    for i in 0..8 {
        initial.push_str(&format!("| proj{i} | ~/Lab/proj{i} | cos |\n"));
    }
    std::fs::write(&index_file, &initial).unwrap();

    let mut handles = Vec::new();
    for i in 0..8 {
        let t = Arc::clone(&temp);
        let handle = thread::spawn(move || {
            let slug = format!("proj{i}");
            let owner = format!("lead-{i}");
            update_project_index_owner(&t, &slug, &owner)
        });
        handles.push(handle);
    }

    for handle in handles {
        let res = handle.join().expect("thread panicked");
        assert!(res.is_ok(), "thread failed update: {:?}", res);
    }

    let final_content = std::fs::read_to_string(&index_file).unwrap();
    for i in 0..8 {
        assert!(
            final_content.contains(&format!("| proj{i} | ~/Lab/proj{i} | lead-{i} |")),
            "missing update for proj{i}: {final_content}"
        );
    }
    assert!(!temp.join("projects/INDEX.md.lock.d").exists());

    let _ = std::fs::remove_dir_all(&*temp);
}

#[test]
fn test_state_atomic_write_and_metadata_roundtrip() {
    use quaqua::state::{atomic_write, ExecutionState, WorkerMetadata};

    let temp = std::env::temp_dir().join(format!("quaqua-state-test-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();

    // 1. atomic_write
    let p = temp.join("test_atomic.txt");
    atomic_write(&p, "hello atomic").unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello atomic");

    // 2. WorkerMetadata load/save
    WorkerMetadata::save_owner(&temp, "worker1", "mylead").unwrap();
    assert_eq!(WorkerMetadata::load_owner(&temp, "worker1"), "mylead");
    assert_eq!(WorkerMetadata::load_owner(&temp, "nonexistent"), "cos");

    WorkerMetadata::save_grid(&temp, "worker1", "my-tab").unwrap();
    assert_eq!(
        WorkerMetadata::load_grid(&temp, "worker1"),
        Some("my-tab".to_string())
    );
    assert_eq!(WorkerMetadata::load_grid(&temp, "nonexistent"), None);

    assert_eq!(WorkerMetadata::load_elapsed(&temp, "worker1"), 0);
    WorkerMetadata::save_elapsed(&temp, "worker1", 120).unwrap();
    assert_eq!(WorkerMetadata::load_elapsed(&temp, "worker1"), 120);

    assert_eq!(WorkerMetadata::load_progress(&temp, "worker1"), 0);
    WorkerMetadata::save_progress(&temp, "worker1", 12345).unwrap();
    assert_eq!(WorkerMetadata::load_progress(&temp, "worker1"), 12345);

    assert!(!WorkerMetadata::has_nudged(&temp, "worker1"));
    WorkerMetadata::record_nudged(&temp, "worker1").unwrap();
    assert!(WorkerMetadata::has_nudged(&temp, "worker1"));

    // 3. ExecutionState
    let pipe_file = temp.join("pipeline-myfeature.state");
    std::fs::write(&pipe_file, "lead=lead1\nbuilder=sude\nfinished=0\n").unwrap();
    let es = ExecutionState::load(&pipe_file).expect("must parse pipeline state");
    assert!(!es.is_finished());
    assert_eq!(es.owner(), "lead1");

    // A file without finished=0 counts as finished (exact base cap.rs parity)
    let pipe_no_fin = temp.join("pipeline-nofin.state");
    std::fs::write(&pipe_no_fin, "lead=lead1\nbuilder=sude\n").unwrap();
    let es_no_fin = ExecutionState::load(&pipe_no_fin).expect("must parse pipeline state");
    assert!(
        es_no_fin.is_finished(),
        "missing finished=0 must be considered finished"
    );

    let tf_file = temp.join("taskforce-eval.state");
    std::fs::write(&tf_file, "task=eval\nowner=cos\nfinished=1\n").unwrap();
    let es_tf = ExecutionState::load(&tf_file).expect("must parse taskforce state");
    assert!(es_tf.is_finished());
    assert_eq!(es_tf.owner(), "cos");

    // advisor- is not part of taskforce state files
    let adv_file = temp.join("advisor-test.state");
    std::fs::write(&adv_file, "task=eval\nowner=cos\nfinished=0\n").unwrap();
    assert_eq!(ExecutionState::load(&adv_file), None);
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_report_lessons_heading_matcher_and_numbered_headings() {
    use quaqua::worker::prompt::{is_lessons_heading, validate_report_content};

    // 1. Heading line matcher: exact and numbered
    assert!(is_lessons_heading("## Lessons"));
    assert!(is_lessons_heading("## Lessons\n"));
    assert!(is_lessons_heading("## Lessons - none"));
    assert!(is_lessons_heading("## Lessons:"));
    assert!(is_lessons_heading("## 5. Lessons"));
    assert!(is_lessons_heading("## 5. Lessons\n"));
    assert!(is_lessons_heading("## 5. Lessons:"));
    assert!(is_lessons_heading("## 12. Lessons"));
    assert!(is_lessons_heading("## 1.  Lessons"));
    assert!(is_lessons_heading("## 5) Lessons"));
    assert!(is_lessons_heading("  ## 5. Lessons"));

    // Non-matching lines
    assert!(!is_lessons_heading("## Blockers"));
    assert!(!is_lessons_heading("## 5. Blockers"));
    assert!(!is_lessons_heading("## Summary"));
    assert!(!is_lessons_heading("### 5. Lessons")); // h3, not h2
    assert!(!is_lessons_heading("# 5. Lessons")); // h1, not h2
    assert!(!is_lessons_heading("## 5."));
    assert!(!is_lessons_heading("## LessonsLearned"));
    assert!(!is_lessons_heading("Just text with ## 5. Lessons in body"));

    // 2. Full report validation: standard worker report
    let report_exact = "# run report\n\n## Blockers\nnone\n\n## Lessons\n- none\n";
    assert_eq!(validate_report_content(report_exact, false), Ok(()));

    let report_numbered = "# run report\n\n## 1. Summary\nDone\n\n## 2. Blockers\nnone\n\n## 5. Lessons\n- lesson learned\n";
    assert_eq!(validate_report_content(report_numbered, false), Ok(()));

    let report_missing_lessons =
        "# run report\n\n## Blockers\nnone\n\n## Summary\nNo lessons section\n";
    assert_eq!(
        validate_report_content(report_missing_lessons, false),
        Err("'## Lessons' section")
    );

    let report_h3_only =
        "# run report\n\n## Blockers\nnone\n\n### 5. Lessons\n- sub-heading only\n";
    assert_eq!(
        validate_report_content(report_h3_only, false),
        Err("'## Lessons' section")
    );

    // 3. Reviewer report validation: requires Verdict:
    let reviewer_valid = "# review report\n\nVerdict: approved\n";
    assert_eq!(validate_report_content(reviewer_valid, true), Ok(()));

    let reviewer_missing = "# review report\n\n## Lessons\n- none\n";
    assert_eq!(
        validate_report_content(reviewer_missing, true),
        Err("'Verdict:' line")
    );
}

#[test]
fn test_parse_prompt_args() {
    use quaqua::worker::prompt::{parse_prompt_args, PromptAction};

    // --check-brief
    let cb = vec![
        "--check-brief".to_string(),
        "Goal: g\nHard constraints: none\nCurrent design: d".to_string(),
    ];
    assert_eq!(
        parse_prompt_args(&cb),
        Ok(PromptAction::CheckBrief(
            "Goal: g\nHard constraints: none\nCurrent design: d".to_string()
        ))
    );

    // --resume without extend
    let res = vec!["--resume".to_string(), "sude-w4a".to_string()];
    assert_eq!(
        parse_prompt_args(&res),
        Ok(PromptAction::Resume {
            name: "sude-w4a".to_string(),
            extend: None,
        })
    );

    // --resume with extend
    let res_ext = vec![
        "--resume".to_string(),
        "sude-w4a".to_string(),
        "--extend".to_string(),
        "600".to_string(),
    ];
    assert_eq!(
        parse_prompt_args(&res_ext),
        Ok(PromptAction::Resume {
            name: "sude-w4a".to_string(),
            extend: Some(600),
        })
    );

    // Normal prompt
    let norm = vec![
        "sude-w4a".to_string(),
        "Goal: g\nHard constraints: none\nCurrent design: d".to_string(),
    ];
    assert_eq!(
        parse_prompt_args(&norm),
        Ok(PromptAction::Prompt {
            name: "sude-w4a".to_string(),
            task: "Goal: g\nHard constraints: none\nCurrent design: d".to_string(),
        })
    );

    // Missing brief headers
    let bad_brief = vec!["sude-w4a".to_string(), "just do this".to_string()];
    assert!(parse_prompt_args(&bad_brief).is_err());
}

#[test]
fn test_worktree_paths_and_branch_naming() {
    use quaqua::worker::worktree::{
        canonicalize_repo_path, resolve_paths, resolve_worktree_path, worker_branch_name,
    };

    assert_eq!(worker_branch_name("sude-w4b"), "quaqua/sude-w4b");

    let repo = Path::new("/path/to/myrepo");
    let wt = resolve_worktree_path(repo, "sude-task1");
    assert_eq!(wt, PathBuf::from("/path/to/.wt/sude-task1"));

    let no_parent = Path::new("repo");
    let wt_no_parent = resolve_worktree_path(no_parent, "sude-task1");
    assert_eq!(wt_no_parent, PathBuf::from(".wt/sude-task1"));

    let (r, w) = resolve_paths(repo, "sude-task1");
    assert_eq!(r, canonicalize_repo_path(repo));
    assert_eq!(w, resolve_worktree_path(&r, "sude-task1"));
}

#[test]
fn test_worktree_validate_base_ref() {
    use quaqua::worker::worktree::validate_base_ref;

    struct BaseRefTestGit(bool);
    impl GitOps for BaseRefTestGit {
        fn run_in(&self, _dir: &Path, _args: &[&str]) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn run(&self, _args: &[&str]) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn rev_parse_commit(&self, _repo: &Path, _base: &str) -> bool {
            self.0
        }
        fn worktree_list_porcelain(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
            unimplemented!()
        }
        fn worktree_add(
            &self,
            _repo: &Path,
            _branch: &str,
            _wt: &Path,
            _base: Option<&str>,
        ) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn branch_exists(&self, _repo: &Path, _branch: &str) -> bool {
            unimplemented!()
        }
        fn worktree_add_existing(
            &self,
            _repo: &Path,
            _branch: &str,
            _wt: &Path,
        ) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn worktree_remove(&self, _repo: &Path, _wt: &Path) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn remote_list(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
            unimplemented!()
        }
        fn is_core_bare(&self, _repo: &Path) -> bool {
            unimplemented!()
        }
        fn has_core_worktree(&self, _repo: &Path) -> bool {
            unimplemented!()
        }
        fn enable_worktree_config(&self, _repo: &Path) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn set_worktree_pushurl_blocked(&self, _wt: &Path, _remote: &str) -> std::io::Result<()> {
            unimplemented!()
        }
        fn is_git_dir(&self, _cwd: &Path) -> bool {
            unimplemented!()
        }
        fn diff_and_status(&self, _cwd: &Path) -> (String, String) {
            unimplemented!()
        }
    }

    let repo = Path::new("/test/repo");
    assert!(validate_base_ref(&BaseRefTestGit(true), repo, "main").is_ok());

    let err = validate_base_ref(&BaseRefTestGit(false), repo, "badref")
        .expect_err("must error on invalid ref");
    assert_eq!(err.0, 1);
    assert!(err
        .1
        .contains("error: --base 'badref' is not a known commit-ish"));
}

#[test]
fn test_worktree_prepare_and_guards() {
    use quaqua::worker::worktree::{
        apply_push_block, is_worktree_dirty, prepare_worktree, remove_worktree,
    };

    struct WtMockGit {
        branch_exists_val: bool,
        porcelain_wts: Vec<String>,
        bare: bool,
        wt_config: bool,
        dirty_output: String,
        remotes: Vec<String>,
        pushurl_blocked: std::cell::RefCell<Vec<String>>,
        removed: std::cell::RefCell<bool>,
    }

    impl GitOps for WtMockGit {
        fn run_in(&self, _dir: &Path, args: &[&str]) -> std::io::Result<Output> {
            let out_str = if args.contains(&"status") {
                self.dirty_output.clone()
            } else {
                String::new()
            };
            Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: out_str.into_bytes(),
                stderr: Vec::new(),
            })
        }
        fn run(&self, _args: &[&str]) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn rev_parse_commit(&self, _repo: &Path, _base: &str) -> bool {
            true
        }
        fn worktree_list_porcelain(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
            Ok(self.porcelain_wts.clone())
        }
        fn worktree_add(
            &self,
            _repo: &Path,
            _branch: &str,
            wt: &Path,
            _base: Option<&str>,
        ) -> std::io::Result<Output> {
            let _ = std::fs::create_dir_all(wt);
            Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn branch_exists(&self, _repo: &Path, _branch: &str) -> bool {
            self.branch_exists_val
        }
        fn worktree_add_existing(
            &self,
            _repo: &Path,
            _branch: &str,
            wt: &Path,
        ) -> std::io::Result<Output> {
            let _ = std::fs::create_dir_all(wt);
            Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn worktree_remove(&self, _repo: &Path, wt: &Path) -> std::io::Result<Output> {
            let _ = std::fs::remove_dir_all(wt);
            *self.removed.borrow_mut() = true;
            Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn remote_list(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
            Ok(self.remotes.clone())
        }
        fn is_core_bare(&self, _repo: &Path) -> bool {
            self.bare
        }
        fn has_core_worktree(&self, _repo: &Path) -> bool {
            self.wt_config
        }
        fn enable_worktree_config(&self, _repo: &Path) -> std::io::Result<Output> {
            Ok(Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn set_worktree_pushurl_blocked(&self, _wt: &Path, remote: &str) -> std::io::Result<()> {
            self.pushurl_blocked.borrow_mut().push(remote.to_string());
            Ok(())
        }
        fn is_git_dir(&self, _cwd: &Path) -> bool {
            true
        }
        fn diff_and_status(&self, _cwd: &Path) -> (String, String) {
            ("".to_string(), "".to_string())
        }
    }

    let temp = std::env::temp_dir().join(format!("quaqua-wt-unit-{}", std::process::id()));
    let repo = temp.join("repo");
    let wt = temp.join(".wt").join("sude-testwt");
    let _ = std::fs::create_dir_all(&repo);

    // 1. Bare guard triggers error
    let git_bare = WtMockGit {
        branch_exists_val: false,
        porcelain_wts: vec![],
        bare: true,
        wt_config: false,
        dirty_output: "".to_string(),
        remotes: vec!["origin".to_string()],
        pushurl_blocked: std::cell::RefCell::new(Vec::new()),
        removed: std::cell::RefCell::new(false),
    };
    let err = apply_push_block(&git_bare, &repo, &wt).expect_err("bare must fail");
    assert_eq!(err.0, 1);
    assert!(err.1.contains("core.bare=true or core.worktree"));

    // 2. Foreign dir at wt path rejected
    let _ = std::fs::create_dir_all(&wt);
    let git_foreign = WtMockGit {
        branch_exists_val: false,
        porcelain_wts: vec![], // wt is not in porcelain worktree list!
        bare: false,
        wt_config: false,
        dirty_output: "".to_string(),
        remotes: vec!["origin".to_string()],
        pushurl_blocked: std::cell::RefCell::new(Vec::new()),
        removed: std::cell::RefCell::new(false),
    };
    let err = prepare_worktree(&git_foreign, &repo, &wt, "sude-testwt", None)
        .expect_err("foreign dir must fail");
    assert_eq!(err.0, 1);
    assert!(err.1.contains("exists but is not a git worktree"));

    // 3. Registered dir reused
    let git_reg = WtMockGit {
        branch_exists_val: false,
        porcelain_wts: vec![wt.to_string_lossy().to_string()],
        bare: false,
        wt_config: false,
        dirty_output: "".to_string(),
        remotes: vec!["origin".to_string(), "upstream".to_string()],
        pushurl_blocked: std::cell::RefCell::new(Vec::new()),
        removed: std::cell::RefCell::new(false),
    };
    assert!(prepare_worktree(&git_reg, &repo, &wt, "sude-testwt", None).is_ok());
    assert_eq!(git_reg.pushurl_blocked.borrow().len(), 2);

    // 4. Dirty check
    let git_clean = WtMockGit {
        branch_exists_val: false,
        porcelain_wts: vec![],
        bare: false,
        wt_config: false,
        dirty_output: "".to_string(),
        remotes: vec![],
        pushurl_blocked: std::cell::RefCell::new(Vec::new()),
        removed: std::cell::RefCell::new(false),
    };
    assert_eq!(is_worktree_dirty(&git_clean, &wt), Ok(false));

    let git_dirty = WtMockGit {
        branch_exists_val: false,
        porcelain_wts: vec![],
        bare: false,
        wt_config: false,
        dirty_output: " M file.rs\n?? new.rs\n".to_string(),
        remotes: vec![],
        pushurl_blocked: std::cell::RefCell::new(Vec::new()),
        removed: std::cell::RefCell::new(false),
    };
    assert_eq!(is_worktree_dirty(&git_dirty, &wt), Ok(true));

    // 5. Worktree remove
    assert!(remove_worktree(&git_clean, &repo, &wt).is_ok());
    assert!(*git_clean.removed.borrow());

    let _ = std::fs::remove_dir_all(&temp);
}

// ---------------------------------------------------------------------------
// Cap-gate coverage that used to live in the retired Nhóm-1 bash suites: the
// gate counts live task workers per owner, exempts project Leads, and charges
// a pipeline or a dual-lens taskforce 2 slots. Every spawn path calls the same
// check, so a refusal arrives before a pane exists and names the cap that fired.
// ---------------------------------------------------------------------------

/// A `herdr agent list` answer with every name live (`working`).
fn agent_list_json(names: &[&str]) -> String {
    let agents: Vec<String> = names
        .iter()
        .map(|n| format!(r#"{{"name":"{n}","agent_status":"working"}}"#))
        .collect();
    format!(r#"{{"result":{{"agents":[{}]}}}}"#, agents.join(","))
}

/// A fake herdr that answers `agent list` with `json` and otherwise behaves like
/// a spawn target (so the caller's pane query yields no owner tag).
fn herdr_listing(json: &str) -> MockActivitiesHerdr {
    let mut mock = MockActivities::new();
    mock.register(
        "herdr.agent_list",
        ActivityResult::Herdr(HerdrResult::AgentOutput {
            content: json.to_string(),
        }),
    );
    mock.register(
        "herdr.pane_split",
        ActivityResult::Herdr(HerdrResult::AgentOutput {
            content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
        }),
    );
    mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
        name: String::new(),
    }));
    MockActivitiesHerdr::new(mock)
}

#[test]
fn test_cap_gate_counts_live_workers_and_exempts_leads() {
    let _env_lock = lock_env();
    let tmp = std::env::temp_dir().join(format!("quaqua-cap-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let repo_dir = tmp.join("repo");
    let runs_dir = tmp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");
    std::env::set_var("QUAQUA_OWNER", "cos");

    let cfg = config::embedded_team_config().expect("embedded team.toml");
    let cap = quaqua::cap::owner_cap(&cfg, "cos");
    let repo = repo_dir.to_str().unwrap().to_string();

    // 1. The cap is full: `cap` live CoS workers plus two project Leads, which
    //    are not task workers and charge nobody.
    let workers: Vec<String> = (1..=cap).map(|i| format!("sude-w{i}")).collect();
    let mut names: Vec<&str> = workers.iter().map(|w| w.as_str()).collect();
    names.extend(["coucou-lead", "baro-lead"]);
    for w in &workers {
        std::fs::write(runs_dir.join(format!("{w}.owner")), "cos\n").unwrap();
    }
    let herdr = herdr_listing(&agent_list_json(&names));
    assert_eq!(
        count_owner_slots(&runs_dir, "cos", None, &herdr, &cfg),
        cap,
        "live workers owned by the CoS fill its cap"
    );
    assert_eq!(
        count_owner_slots(&runs_dir, "coucou-lead", None, &herdr, &cfg),
        0,
        "a Lead pane must never charge another owner"
    );

    // 2. The spawn path refuses the next slot before any pane exists.
    let opts = parse_spawn_args(&[
        "sude-overflow".to_string(),
        "sude".to_string(),
        repo.clone(),
    ])
    .unwrap();
    let (code, msg) =
        quaqua::worker::spawn::spawn_worker(opts, &cfg, &SpawnTestGit, &herdr, quaqua::layout::SpawnOrigin::Cli).unwrap_err();
    assert_eq!(code, 1, "cap refusal must exit 1: {msg}");
    assert!(msg.contains("worker cap exceeded"), "{msg}");
    assert!(
        msg.contains(&format!("{cap} live worker slots for owner 'cos'")),
        "{msg}"
    );
    assert!(
        !herdr.has_call("herdr.pane_split"),
        "a refused spawn must not split a pane"
    );

    // 3. Owner isolation: the CoS being full does not block a Lead's spawn.
    std::env::set_var("QUAQUA_OWNER", "coucou-lead");
    let opts = parse_spawn_args(&[
        "sude-leadw1".to_string(),
        "sude".to_string(),
        repo.clone(),
    ])
    .unwrap();
    let herdr = herdr_listing(&agent_list_json(&names));
    quaqua::worker::spawn::spawn_worker(opts, &cfg, &SpawnTestGit, &herdr, quaqua::layout::SpawnOrigin::Cli)
        .expect("a Lead spawns despite the CoS sitting at its cap");
    assert_eq!(
        std::fs::read_to_string(runs_dir.join("sude-leadw1.owner")).unwrap(),
        "coucou-lead\n"
    );

    // 4. A Lead's own Peers stop at `[roles.lead] max_peers`; the CoS does not
    //    inherit that cap.
    let peers = quaqua::cap::owner_cap(&cfg, "baro-lead");
    let peer_names: Vec<String> = (1..=peers).map(|i| format!("sude-p{i}")).collect();
    let mut names: Vec<&str> = peer_names.iter().map(|w| w.as_str()).collect();
    names.extend(["coucou-lead", "baro-lead"]);
    for w in &peer_names {
        std::fs::write(runs_dir.join(format!("{w}.owner")), "baro-lead\n").unwrap();
    }
    std::env::set_var("QUAQUA_OWNER", "baro-lead");
    let herdr = herdr_listing(&agent_list_json(&names));
    let opts = parse_spawn_args(&[
        "sude-p-overflow".to_string(),
        "sude".to_string(),
        repo.clone(),
    ])
    .unwrap();
    let (code, msg) =
        quaqua::worker::spawn::spawn_worker(opts, &cfg, &SpawnTestGit, &herdr, quaqua::layout::SpawnOrigin::Cli).unwrap_err();
    assert_eq!(code, 1, "{msg}");
    assert!(msg.contains("owner 'baro-lead'"), "{msg}");
    assert!(
        msg.contains(&format!("roles.lead.max_peers is {peers}")),
        "{msg}"
    );

    std::env::set_var("QUAQUA_OWNER", "cos");
    let opts = parse_spawn_args(&[
        "sude-cosfree".to_string(),
        "sude".to_string(),
        repo,
    ])
    .unwrap();
    let herdr = herdr_listing(&agent_list_json(&names));
    quaqua::worker::spawn::spawn_worker(opts, &cfg, &SpawnTestGit, &herdr, quaqua::layout::SpawnOrigin::Cli)
        .expect("the Lead cap must not leak into the CoS");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_cap_gate_charges_a_pipeline_two_slots() {
    let tmp = std::env::temp_dir().join(format!("quaqua-cap-pipe2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let cfg = config::embedded_team_config().expect("embedded team.toml");
    let cap = quaqua::cap::owner_cap(&cfg, "cos");
    let pipe_weight = cfg.limits.weights.get("pipeline").copied().unwrap_or(2);

    // One active pipeline (2 slots) plus `cap - 2` standalone workers fills the cap.
    let solo: Vec<String> = (1..=(cap - pipe_weight)).map(|i| format!("sude-s{i}")).collect();
    for w in &solo {
        std::fs::write(tmp.join(format!("{w}.owner")), "cos\n").unwrap();
    }
    std::fs::write(
        tmp.join("pipeline-feature.state"),
        format!(
            "task=feature\nbuilder=sude\nlead=cos\nfinished=0\npipeline_pid={}\n",
            std::process::id()
        ),
    )
    .unwrap();
    let names: Vec<&str> = solo.iter().map(|w| w.as_str()).collect();
    let herdr = herdr_listing(&agent_list_json(&names));

    assert_eq!(
        count_owner_slots(&tmp, "cos", None, &herdr, &cfg),
        cap,
        "a pipeline counts {pipe_weight} slots, not 1"
    );
    let err = quaqua::cap::check_owner_cap(&cfg, "cos", cap, 1, "this spawn").unwrap_err();
    assert!(err.contains("worker cap exceeded"), "{err}");
    // One slot short of the cap, one worker still fits.
    assert!(quaqua::cap::check_owner_cap(&cfg, "cos", cap - 1, 1, "this spawn").is_ok());

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The retired CoS label must not reappear in the tree: the rename is only
/// done when no shipped file still names the old role. Dated `memory/` logs,
/// the CoS-owned `AGENTS.md` template, the research notes, and the test tree
/// (which has to spell the pattern to ban it) are history, not live contracts.
#[test]
fn test_no_dangling_legacy_role_reference() {
    let legacy = concat!("super", "visor");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let skip_dirs = [".git", "target", "memory", "docs"];
    let mut offenders: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !skip_dirs.contains(&name.as_str()) {
                    stack.push(path);
                }
                continue;
            }
            if name == "AGENTS.md" || path.starts_with(root.join("crates/quaqua/tests")) {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue; // binary or non-UTF8: no text role name to find
            };
            scanned += 1;
            if body.contains(legacy) {
                offenders.push(path.display().to_string());
            }
        }
    }
    assert!(
        scanned > 100,
        "the walk found only {scanned} text file(s): it is not scanning the tree"
    );
    assert!(
        offenders.is_empty(),
        "files still name the retired role '{legacy}': {offenders:#?}"
    );
}

// ─── Lead-required gate (AGENTS.md '## Roles (SLP)') ────────────────────────
//
// A `cos`-owned project with >= 2 topology signals refuses the spawn; one
// signal only warns. `--solo "<reason>"` bypasses the block and records why.

use quaqua::worker::leadgate::{self, GateInput, Signal};

/// A temp workspace with one `cos` project (`q` → `<root>/repo`), a plan with
/// `phases_left: 3`, and an empty runs dir.
struct GateFixture {
    root: PathBuf,
    repo: PathBuf,
    runs: PathBuf,
    caller: quaqua::identity::Caller,
}

impl GateFixture {
    fn new(tag: &str, owner: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("quaqua-leadgate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["projects", "docs/plans/active", "memory", "runs", "repo"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let repo = root.join("repo");
        std::fs::write(
            root.join("projects/INDEX.md"),
            format!(
                "# Projects\n\n| slug | repo | owner | status | one-line goal |\n|---|---|---|---|---|\n| q | {} | {owner} | active | engine gate |\n",
                repo.display()
            ),
        )
        .unwrap();
        std::fs::write(root.join("projects/q.md"), "# q\n\nrepo: see INDEX\n").unwrap();
        std::fs::write(
            root.join("docs/plans/active/q.md"),
            "---\nstatus: active\nphases_left: 3\n---\n# q plan\n",
        )
        .unwrap();
        GateFixture {
            repo: root.join("repo"),
            runs: root.join("runs"),
            root,
            caller: quaqua::identity::Caller {
                name: "cos".to_string(),
                role: quaqua::identity::Role::Cos,
                depth: quaqua::identity::CallerDepth::Resolved(0),
                source: quaqua::identity::IdentitySource::PaneTokens,
            },
        }
    }

    fn repo(&self) -> PathBuf {
        self.repo.clone()
    }

    fn runs(&self) -> PathBuf {
        self.runs.clone()
    }

    /// Two live build workers on the repo, attributed by `runs/<name>.repo`.
    fn with_two_live_builders(&self) -> MockActivitiesHerdr {
        for name in ["sude-live1", "tieude-live2"] {
            std::fs::write(
                self.runs().join(format!("{name}.repo")),
                format!("{}\n", self.repo.display()),
            )
            .unwrap();
        }
        let agents = r#"{"result":{"agents":[{"name":"sude-live1","agent_status":"working"},{"name":"tieude-live2","agent_status":"working"}]}}"#.to_string();
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.agent_list",
            ActivityResult::Herdr(HerdrResult::AgentOutput { content: agents }),
        );
        mock.register(
            "herdr.pane_split",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
            }),
        );
        mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
            name: String::new(),
        }));
        MockActivitiesHerdr::new(mock)
    }

    /// An idle herdr: no live agents, a pane split that works.
    fn idle_herdr(&self) -> MockActivitiesHerdr {
        let mut mock = MockActivities::new();
        mock.register(
            "herdr.agent_list",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: r#"{"result":{"agents":[]}}"#.to_string(),
            }),
        );
        mock.register(
            "herdr.pane_split",
            ActivityResult::Herdr(HerdrResult::AgentOutput {
                content: r#"{"result":{"pane":{"pane_id":"%1"}}}"#.to_string(),
            }),
        );
        mock.set_default(ActivityResult::Herdr(HerdrResult::Spawned {
            name: String::new(),
        }));
        MockActivitiesHerdr::new(mock)
    }

    /// Spawn `sude-gate` on the fixture repo through the real spawn path.
    fn spawn(&self, herdr: &MockActivitiesHerdr, extra: &[&str]) -> Result<(), (i32, String)> {
        let mut args: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        args.push("sude-gate".to_string());
        args.push("sude".to_string());
        args.push(self.repo().to_string_lossy().to_string());
        let opts = parse_spawn_args(&args).unwrap();
        quaqua::worker::spawn::spawn_worker(opts, &gate_cfg(), &SpawnTestGit, herdr, quaqua::layout::SpawnOrigin::Cli)
    }

    fn today(&self) -> String {
        leadgate::today_local()
    }
}

impl Drop for GateFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn gate_cfg() -> config::Config {
    let mut cfg = config::embedded_team_config().expect("embedded team.toml");
    cfg.limits.weights.insert("pipeline".to_string(), 2);
    cfg
}

/// The gate's view of a fixture spawn (all borrows live for the call).
fn gate_input<'a>(
    fx: &'a GateFixture,
    repo: &'a Path,
    name: &'a str,
    solo: Option<&'a str>,
) -> GateInput<'a> {
    GateInput {
        root: &fx.root,
        runs_dir: &fx.runs,
        repo,
        name,
        brief: None,
        solo,
        pipeline: None,
        caller: &fx.caller,
    }
}

#[test]
fn lead_gate_blocks_third_worker_on_a_multi_phase_cos_project() {
    let _env_lock = lock_env();
    let fx = GateFixture::new("block", "cos");
    let herdr = fx.with_two_live_builders();
    let repo = fx.repo();

    let facts = leadgate::gather_facts(
        &gate_input(&fx, &repo, "sude-gate", None),
        &gate_cfg(),
        &herdr,
    )
    .expect("repo maps to a cos-owned project");
    assert_eq!(facts.slug, "q");
    assert!(
        facts
            .signals
            .iter()
            .any(|s| matches!(s, Signal::LiveBuildWorkers(3))),
        "3rd live build worker must be a signal: {:?}",
        facts.signals
    );
    assert!(
        facts
            .signals
            .iter()
            .any(|s| matches!(s, Signal::PhasesLeft { left: 3, .. })),
        "phases_left 3 must be a signal: {:?}",
        facts.signals
    );

    let err = leadgate::check_spawn(
        gate_input(&fx, &repo, "sude-gate", None),
        &gate_cfg(),
        &herdr,
    )
    .expect_err("two signals must block the spawn");
    assert_eq!(err.0, 1);
    assert!(err.1.contains("needs a Lead"), "{}", err.1);
    assert!(err.1.contains("quaqua lead spawn q"), "{}", err.1);
    assert!(err.1.contains("phases_left 3"), "{}", err.1);
    assert!(err.1.contains("live build worker slots"), "{}", err.1);
}
#[test]
fn lead_gate_passes_when_caller_is_project_lead_even_if_repo_has_cos_row() {
    let _env_lock = lock_env();
    let fx = GateFixture::new("leadcaller", "cos");
    let herdr = fx.with_two_live_builders();
    let repo = fx.repo();

    // Add a row for the running lead on the same repo in INDEX.md
    let index_path = fx.root.join("projects/INDEX.md");
    let current_index = std::fs::read_to_string(&index_path).unwrap();
    let updated_index = format!("{current_index}| gate | {} | gate-lead | active | gate goal |\n", repo.display());
    std::fs::write(&index_path, updated_index).unwrap();

    let lead_caller = quaqua::identity::Caller {
        name: "gate-lead".to_string(),
        role: quaqua::identity::Role::Lead,
        depth: quaqua::identity::CallerDepth::Resolved(1),
        source: quaqua::identity::IdentitySource::PaneTokens,
    };

    let mut input = gate_input(&fx, &repo, "sude-gate", None);
    input.caller = &lead_caller;

    assert_eq!(
        leadgate::gather_facts(&input, &gate_cfg(), &herdr),
        None,
        "lead caller must not gather facts on a project"
    );

    assert!(
        leadgate::check_spawn(input, &gate_cfg(), &herdr).is_ok(),
        "lead caller must not be blocked by leadgate"
    );
}
#[test]
fn lead_gate_blocks_lead_on_foreign_cos_project() {
    let _env_lock = lock_env();
    let fx = GateFixture::new("foreignlead", "cos");
    let herdr = fx.with_two_live_builders();
    let repo = fx.repo();

    let foreign_lead = quaqua::identity::Caller {
        name: "foreign-lead".to_string(),
        role: quaqua::identity::Role::Lead,
        depth: quaqua::identity::CallerDepth::Resolved(1),
        source: quaqua::identity::IdentitySource::PaneTokens,
    };

    let mut input = gate_input(&fx, &repo, "sude-gate", None);
    input.caller = &foreign_lead;

    let facts = leadgate::gather_facts(&input, &gate_cfg(), &herdr);
    assert!(facts.is_some(), "lead on foreign cos repo must gather facts");

    let err = leadgate::check_spawn(input, &gate_cfg(), &herdr)
        .expect_err("lead on foreign cos repo with 2 signals must be blocked");
    assert_eq!(err.0, 1);
    assert!(err.1.contains("needs a Lead"), "{}", err.1);
}

#[test]
fn lead_gate_solo_override_lets_the_spawn_through_and_records_it() {
    let _env_lock = lock_env();
    let fx = GateFixture::new("solo", "cos");
    std::env::set_var("QUAQUA_RUNS_DIR", fx.runs());
    std::env::set_var("QUAQUA_HOME", &fx.root);
    std::env::set_var("QUAQUA_TEST", "1");
    std::env::set_var("GRID_SKIP", "1");
    let herdr = fx.with_two_live_builders();

    let res = fx.spawn(&herdr, &["--solo", "one engine file, no Lead"]);
    assert!(
        res.is_ok(),
        "solo must let the spawn through: {:?}",
        res.err()
    );
    assert!(
        herdr.agent_starts().iter().any(|(n, _)| n == "sude-gate"),
        "the worker must actually start"
    );

    let memory = std::fs::read_to_string(fx.root.join("memory").join(format!("{}.md", fx.today())))
        .expect("solo override must be recorded in today's memory file");
    assert!(
        memory.contains("- [q] solo override: one engine file, no Lead"),
        "{memory}"
    );
}

#[test]
fn check_brief_requires_a_topology_header() {
    use quaqua::worker::prompt::do_check_brief;

    let _env_lock = lock_env();
    let with =
        "Topology: solo — one engine task\nGoal: g\nHard constraints: none\nCurrent design: d\n";
    assert!(
        do_check_brief(with).is_ok(),
        "a declared topology must pass"
    );

    let without = "Goal: g\nHard constraints: none\nCurrent design: d\n";
    let err = do_check_brief(without).expect_err("a brief without Topology is refused");
    assert_eq!(err.0, 1);
    assert!(err.1.contains("Topology: solo|lead"), "{}", err.1);
}

#[test]
fn lead_gate_refuses_a_short_solo_reason() {
    let err = match parse_spawn_args(&[
        "--solo".to_string(),
        "short".to_string(),
        "sude-gate".to_string(),
        "sude".to_string(),
        "/repo".to_string(),
    ]) {
        Ok(_) => panic!("a reason under 10 chars must be refused"),
        Err(e) => e,
    };
    assert_eq!(err.0, 1);
    assert!(err.1.contains("at least 10 characters"), "{}", err.1);
}

#[test]
fn lead_gate_is_silent_for_a_lead_owned_project() {
    let _env_lock = lock_env();
    let fx = GateFixture::new("leadown", "q-lead");
    let herdr = fx.with_two_live_builders();
    let repo = fx.repo();
    assert!(
        leadgate::gather_facts(
            &gate_input(&fx, &repo, "sude-gate", None),
            &gate_cfg(),
            &herdr
        )
        .is_none(),
        "a project already owned by its Lead must not be gated"
    );
    assert!(leadgate::check_spawn(
        gate_input(&fx, &repo, "sude-gate", None),
        &gate_cfg(),
        &herdr
    )
    .is_ok());
}

#[test]
fn lead_gate_is_silent_for_a_repo_missing_from_the_index() {
    let _env_lock = lock_env();
    let fx = GateFixture::new("noindex", "cos");
    let herdr = fx.with_two_live_builders();
    let elsewhere =
        std::env::temp_dir().join(format!("quaqua-leadgate-elsewhere-{}", std::process::id()));
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert!(
        leadgate::gather_facts(
            &gate_input(&fx, &elsewhere, "sude-gate", None),
            &gate_cfg(),
            &herdr
        )
        .is_none(),
        "a repo with no INDEX row must not be gated"
    );
    assert!(leadgate::check_spawn(
        gate_input(&fx, &elsewhere, "sude-gate", None),
        &gate_cfg(),
        &herdr
    )
    .is_ok());
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn lead_gate_warns_on_a_single_signal_and_spawns() {
    let _env_lock = lock_env();
    let fx = GateFixture::new("warn", "cos");
    let herdr = fx.idle_herdr();
    let repo = fx.repo();

    let facts = leadgate::gather_facts(
        &gate_input(&fx, &repo, "sude-gate", None),
        &gate_cfg(),
        &herdr,
    )
    .expect("repo maps to a cos-owned project");
    assert_eq!(facts.signals.len(), 1, "{:?}", facts.signals);
    assert_eq!(
        leadgate::verdict(facts.signals.clone()),
        leadgate::Verdict::Warn(facts.signals)
    );
    assert!(
        leadgate::check_spawn(
            gate_input(&fx, &repo, "sude-gate", None),
            &gate_cfg(),
            &herdr
        )
        .is_ok(),
        "one signal only warns"
    );
}
