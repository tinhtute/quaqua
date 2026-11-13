//! Worker cleanup logic (`quaqua worker cleanup <name> <repo-path>`).

use crate::engine::journal::Journal;
use crate::git::GitOps;
use crate::herdr::HerdrOps;
use crate::liveness::{liveness, Liveness};
use crate::state::WorkerMetadata;
use crate::worker::prompt::cost::Tokens;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Worker telemetry record written to `runs/telemetry.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerTelemetryRow {
    pub ts: String,
    pub rec: String,
    pub name: String,
    pub kind: Option<String>,
    pub harness: Option<String>,
    pub owner: Option<String>,
    pub pipeline: Option<String>,
    pub repo: Option<String>,
    #[serde(default)]
    pub prompts: Option<u32>,
    pub exit: Option<i32>,
    pub wall_sec: Option<u64>,
    pub budget_sec: Option<u64>,
    pub usd: Option<f64>,
    pub usd_budget: Option<f64>,
    pub tokens: Option<Tokens>,
    pub spend_src: String,
    pub model: Option<String>,
}

pub fn format_repo_path(repo: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            let home_path = Path::new(&home);
            if let Ok(rel) = repo.strip_prefix(home_path) {
                if rel.as_os_str().is_empty() {
                    return "~".to_string();
                } else {
                    return format!("~/{}", rel.display());
                }
            }
            if let Ok(canon_home) = std::fs::canonicalize(home_path) {
                if let Ok(rel) = repo.strip_prefix(&canon_home) {
                    if rel.as_os_str().is_empty() {
                        return "~".to_string();
                    } else {
                        return format!("~/{}", rel.display());
                    }
                }
            }
        }
    }
    repo.to_string_lossy().into_owned()
}

/// `worker prompt` exit code for a spent time budget.
pub const WATCHDOG_BUDGET_EXIT: i32 = 4;

/// Records the telemetry row of a worker the watchdog stopped (exit 4), so
/// `status --metrics` shows it before any cleanup. Best effort, like cleanup's.
pub fn record_stop_telemetry<H: HerdrOps>(name: &str, repo: &Path, herdr: &H, runs_dir: &Path) {
    let wt = match repo.parent() {
        Some(parent) => parent.join(".wt").join(name),
        None => PathBuf::from(".wt").join(name),
    };
    let agent_info = match liveness(herdr, name) {
        Liveness::Alive(body) => serde_json::from_str::<Value>(&body).ok(),
        _ => None,
    };
    let owner = WorkerMetadata::load_owner(runs_dir, name);
    let owner = (!owner.is_empty()).then_some(owner);
    append_worker_telemetry(
        name,
        repo,
        &wt,
        agent_info.as_ref(),
        owner.as_deref(),
        runs_dir,
    );
}

/// Appends one worker telemetry row to `runs/telemetry.jsonl`.
///
/// Never fails or blocks cleanup: all errors are handled gracefully and missing
/// fields become `null` with `spend_src` explaining the reason.
pub fn append_worker_telemetry(
    name: &str,
    repo: &Path,
    wt: &Path,
    agent_info: Option<&Value>,
    owner: Option<&str>,
    runs_dir: &Path,
) {
    let _ = (|| -> Result<(), ()> {
        let tele_path = runs_dir.join("telemetry.jsonl");
        let mut since_ms: Option<u64> = None;
        // Exit of the newest earlier row for this worker (a watchdog stop row).
        let mut prev_exit: Option<i32> = None;
        if let Ok(file) = std::fs::File::open(&tele_path) {
            use std::io::BufRead;
            for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
                if !line.contains("\"rec\":\"worker\"") || !line.contains(name) {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    if v.get("rec").and_then(|r| r.as_str()) == Some("worker")
                        && v.get("name").and_then(|n| n.as_str()) == Some(name)
                    {
                        if let Some(ts_str) = v.get("ts").and_then(|t| t.as_str()) {
                            if let Some(ms) =
                                crate::worker::prompt::cost::parse_iso_timestamp_ms(ts_str)
                            {
                                since_ms = Some(ms);
                            }
                        }
                        prev_exit = v.get("exit").and_then(|e| e.as_i64()).map(|e| e as i32);
                    }
                }
            }
        }

        let mut harness: Option<String> = None;
        let mut session_id = String::new();
        let mut cwd = wt.to_string_lossy().to_string();

        if let Some(json) = agent_info {
            if let Some(agent) = json.get("result").and_then(|r| r.get("agent")) {
                if let Some(h) = agent
                    .get("agent")
                    .or_else(|| agent.get("harness"))
                    .and_then(|v| v.as_str())
                {
                    if !h.is_empty() {
                        harness = Some(h.to_string());
                    }
                }
                if let Some(s) = agent
                    .get("agent_session")
                    .and_then(|s| s.get("value"))
                    .and_then(|v| v.as_str())
                {
                    session_id = s.to_string();
                }
                if let Some(c) = agent.get("cwd").and_then(|v| v.as_str()) {
                    if !c.is_empty() {
                        cwd = c.to_string();
                    }
                }
            }
        }

        let pipeline = WorkerMetadata::load_pipeline(runs_dir, name);

        let db_path = runs_dir.join("quaqua.db");
        let mut prompts = 0u32;
        let mut last_exit: Option<i32> = None;
        let mut wall_sec: Option<u64> = None;
        let mut first_ts_ms: Option<u64> = None;
        let mut kind: Option<String> = None;
        let mut budget_sec: Option<u64> = None;
        let mut usd_budget: Option<f64> = None;

        if db_path.is_file() {
            if let Ok(journal) = crate::engine::journal_sqlite::SqliteJournal::open(&db_path) {
                for k in 1.. {
                    let wf_id = format!("worker:{name}#{k}");
                    let events = journal.load_events(&wf_id, 1).unwrap_or_default();
                    let snap = journal.load_latest_snapshot(&wf_id).ok().flatten();
                    if events.is_empty() && snap.is_none() {
                        break;
                    }

                    let wf_ts = events.first().map(|e| e.ts_ms);
                    if let Some(fence) = since_ms {
                        if let Some(ts) = wf_ts {
                            if ts < fence {
                                continue;
                            }
                        }
                    }

                    prompts += 1;
                    if first_ts_ms.is_none() {
                        first_ts_ms = wf_ts;
                    }

                    for ev in &events {
                        if ev.kind == "workflow_completed" {
                            if let Ok(out) = serde_json::from_str::<
                                crate::engine::child::SingleWorkerOutput,
                            >(&ev.payload_json)
                            {
                                last_exit = Some(out.exit);
                            }
                        } else if ev.kind == "workflow_failed" {
                            if let Ok(data) =
                                serde_json::from_str::<serde_json::Value>(&ev.payload_json)
                            {
                                if let Some(code) = data.get("code").and_then(|c| c.as_i64()) {
                                    last_exit = Some(code as i32);
                                }
                            }
                        }
                    }

                    if let Some(s) = snap {
                        if let Ok(state) = serde_json::from_str::<
                            crate::worker::prompt::workflow::SingleWorkerState,
                        >(&s.state_json)
                        {
                            kind = Some(state.input.kind.clone());
                            budget_sec = Some(state.input.time_budget_sec);
                            usd_budget = state.input.cost_budget;
                            if harness.is_none() {
                                harness = state.harness.clone();
                            }
                        }
                    }
                }
            }
        }

        // A watchdog stop (exit 4) already recorded this lifecycle; cleanup with
        // no later prompt would only count the same worker twice.
        if prompts == 0 && prev_exit == Some(WATCHDOG_BUDGET_EXIT) {
            return Ok(());
        }

        if kind.is_none() {
            if let Ok(cfg) = crate::config::load_team_config() {
                if let Some(k) = crate::config::kind_of_worker(&cfg, name) {
                    kind = Some(k.to_string());
                }
            }
        }
        if harness.is_none() {
            if let Some(k) = &kind {
                if let Ok(cfg) = crate::config::load_team_config() {
                    if let Some(kind_info) = cfg.kinds.get(k) {
                        harness = Some(kind_info.harness.clone());
                    }
                }
            }
        }

        let now_sec = crate::util::now_sec();
        let now_ms = (now_sec * 1000) as i64;
        if let Some(first_ts) = first_ts_ms {
            if (now_ms as u64) >= first_ts {
                wall_sec = Some((now_ms as u64 - first_ts) / 1000);
            }
        }

        let harness_str = harness.as_deref().unwrap_or("");
        let spend = crate::worker::prompt::cost::read_worker_spend(
            name,
            &session_id,
            &cwd,
            harness_str,
            since_ms,
        );

        let repo_str = format_repo_path(repo);
        let ts_str = crate::usage::format_iso_vn(now_ms);

        let row = WorkerTelemetryRow {
            ts: ts_str,
            rec: "worker".to_string(),
            name: name.to_string(),
            kind,
            harness,
            owner: owner.map(|o| o.to_string()),
            pipeline,
            repo: Some(repo_str),
            prompts: if prompts == 0 { None } else { Some(prompts) },
            exit: last_exit,
            wall_sec,
            budget_sec,
            usd: spend.usd,
            usd_budget,
            tokens: spend.tokens,
            spend_src: spend.spend_src,
            model: spend.model,
        };

        if let Ok(mut json) = serde_json::to_string(&row) {
            json.push('\n');
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(tele_path)
            {
                use std::io::Write;
                let _ = f.write_all(json.as_bytes());
            }
        }

        Ok(())
    })();
}
pub fn resolve_home_quaqua_runs(home: Option<&Path>) -> PathBuf {
    crate::workspace::resolve_workspace_root(None, home).join("runs")
}

pub fn resolve_default_runs_dir(override_runs: Option<&Path>, home: Option<&Path>) -> PathBuf {
    if let Some(dir) = override_runs {
        dir.to_path_buf()
    } else {
        resolve_home_quaqua_runs(home)
    }
}

pub fn home_quaqua_runs() -> PathBuf {
    crate::workspace::workspace_root().join("runs")
}

pub fn default_runs_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("QUAQUA_RUNS_DIR") {
        return PathBuf::from(dir);
    }
    let runs = home_quaqua_runs();
    if crate::workspace::is_test_environment() && crate::workspace::is_live_workspace_runs(&runs) {
        return crate::workspace::test_default_runs_dir();
    }
    runs
}

pub fn run_grid_hook(grid_tab: &str, owner: Option<&str>) -> Result<(), (i32, String)> {
    if std::env::var("GRID_SKIP").as_deref() == Ok("1") {
        let is_pipeline = std::env::var("PIPELINE_TASK").is_ok();
        let is_test = std::env::var("QUAQUA_TEST").as_deref() == Ok("1");
        if is_pipeline || is_test {
            return Ok(());
        }
        eprintln!(
            "warning: GRID_SKIP=1 ignored outside pipeline/tests (rules/_all/fanout-agent-grid.md)"
        );
    }

    if let Err(e) = crate::grid::run_grid_repair(grid_tab, owner) {
        return Err((1, format!("error: grid repair failed: {e}\n")));
    }

    if let Err(e) = crate::grid::run_grid_check(grid_tab, None, owner) {
        return Err((1, format!("error: grid check failed ({e})\n")));
    }

    Ok(())
}

/// The repo to clean `name` against: the explicit argument, else the repo
/// recorded at spawn (`runs/<name>.repo`). `Ok(None)` = cannot infer; an
/// explicit repo that disagrees with the record is an error, so a typo cannot
/// clean (and consume the metadata of) a worker of another repo.
pub fn resolve_cleanup_repo(
    name: &str,
    explicit: Option<&Path>,
    runs_dir: &Path,
) -> Result<Option<PathBuf>, String> {
    let recorded = WorkerMetadata::load_repo(runs_dir, name);
    match (explicit, recorded) {
        (Some(e), Some(r)) => {
            let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            if canon(e) != canon(&r) {
                return Err(format!(
                    "error: repo {} does not match the repo recorded for {name} at spawn ({}) — refusing to clean up\n",
                    e.display(),
                    r.display()
                ));
            }
            Ok(Some(e.to_path_buf()))
        }
        (e, r) => Ok(e.map(Path::to_path_buf).or(r)),
    }
}

pub fn cleanup_worker_in<G: GitOps, H: HerdrOps>(
    name: &str,
    repo_path: &Path,
    git: &G,
    herdr: &H,
    override_runs_dir: Option<&Path>,
) -> Result<(), (i32, String)> {
    let repo = match std::fs::canonicalize(repo_path) {
        Ok(p) => p,
        Err(_) => repo_path.to_path_buf(),
    };

    let wt = match repo.parent() {
        Some(parent) => parent.join(".wt").join(name),
        None => PathBuf::from(".wt").join(name),
    };

    // A Lead has no worker worktree: it is cleaned by `quaqua lead cleanup
    // <slug>`, which also closes the team workspace and returns the project to
    // cos. `is_valid_name` never lets a worker take this name.
    if name.ends_with("-lead") {
        let slug = name.trim_end_matches("-lead");
        return Err((
            1,
            format!("error: {name} is a Lead, not a worker — use `quaqua lead cleanup {slug}`\n"),
        ));
    }

    // agent_not_found alone is not "gone": liveness confirms it with
    // `agent list` before the worktree is removed (issue #69).
    let mut agent_down = false;
    let parsed_json: Option<Value> = match liveness(herdr, name) {
        Liveness::Alive(body) => serde_json::from_str::<Value>(&body).ok(),
        Liveness::Down => {
            agent_down = true; // agent already gone: no pane to close
            None
        }
        Liveness::Unknown(why) => {
            return Err((
                1,
                format!("error: cannot determine whether agent {name} is still live ({why}) — refusing to clean up"),
            ))
        }
    };
    let pane_to_close = parsed_json
        .as_ref()
        .and_then(|j| j.pointer("/result/agent/pane_id"))
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty())
        .map(str::to_string);

    if let Some(pane) = &pane_to_close {
        if crate::cos::pane_is_cos(herdr, pane) {
            return Err((
                1,
                format!("error: refusing to close CoS pane '{pane}' (grid_role=cos)\n"),
            ));
        }
    }

    // "Nothing to remove" may only be reported when this repo proves the worker
    // belonged to it: git still registers a worktree at the derived path, or
    // the spawn's branch survives (cleanup keeps it). A wrong or foreign repo
    // has neither, and must not close the pane, print "cleaned" and delete the
    // record that finds the real worktree (#170 F1).
    let registered = git
        .worktree_list_porcelain(&repo)
        .map_err(|e| {
            (
                1,
                format!("error: git worktree list failed in {}: {e}", repo.display()),
            )
        })?
        .iter()
        .any(|p| Path::new(p) == wt);
    let branch = format!("quaqua/{name}");
    if !wt.is_dir() && !registered && !git.branch_exists(&repo, &branch) {
        // A dead orphan: no agent, no worktree, no branch, and the spawn record
        // proves this repo is the one the worker came from. Only its metadata
        // is left; drop it. Without that proof stay refused (wrong repo?).
        let runs_dir = override_runs_dir
            .map(|p| p.to_path_buf())
            .unwrap_or_else(default_runs_dir);
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let recorded_here = WorkerMetadata::load_repo(&runs_dir, name)
            .map(|r| canon(&r) == repo)
            .unwrap_or(false);
        if agent_down && recorded_here {
            WorkerMetadata::cleanup_worker_files(&runs_dir, name);
            let _ = std::fs::remove_file(runs_dir.join(format!("{name}.base")));
            let _ = std::fs::remove_file(crate::layout::layout_role_path(&runs_dir, name));
            println!("cleaned {name} (orphan: no agent, worktree or branch; metadata removed)");
            return Ok(());
        }
        return Err((
            1,
            format!(
                "error: nothing to remove in {}: no worktree {} and no branch {branch} — refusing to clean up (wrong repo?)\n",
                repo.display(),
                wt.display()
            ),
        ));
    }

    if let Some(pane) = &pane_to_close {
        let _ = herdr.pane_close(pane);
    }

    let runs_dir = override_runs_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_runs_dir);

    let owner = std::env::var("QUAQUA_OWNER")
        .ok()
        .unwrap_or_else(|| WorkerMetadata::load_owner(&runs_dir, name));
    let repo_name = repo.file_name().and_then(|n| n.to_str()).unwrap_or("repo");
    let grid_tab = std::env::var("GRID_TAB")
        .ok()
        .or_else(|| WorkerMetadata::load_grid(&runs_dir, name))
        .unwrap_or_else(|| format!("{repo_name}-workers"));

    // A registration whose directory is already gone is still a worktree to
    // git: `worktree remove` drops it, so a later same-name spawn does not die
    // on "missing but already registered worktree" (#170 F3).
    if wt.is_dir() || registered {
        let rm_out = git
            .worktree_remove(&repo, &wt)
            .map_err(|e| (1, format!("error: worktree remove failed: {e}")))?;

        if !rm_out.status.success() {
            let err_text = String::from_utf8_lossy(&rm_out.stderr).to_string();
            return Err((rm_out.status.code().unwrap_or(1), err_text));
        }
    }

    run_grid_hook(&grid_tab, Some(&owner))?;

    // Record worker telemetry after fallible worktree remove and grid steps,
    // but BEFORE cleanup_worker_files removes state and metadata!
    append_worker_telemetry(
        name,
        &repo,
        &wt,
        parsed_json.as_ref(),
        Some(&owner),
        &runs_dir,
    );

    WorkerMetadata::cleanup_worker_files(&runs_dir, name);
    let _ = std::fs::remove_file(runs_dir.join(format!("{name}.base")));
    let _ = std::fs::remove_file(crate::layout::layout_role_path(&runs_dir, name));
    println!("cleaned {name} (branch quaqua/{name} kept)");
    Ok(())
}

pub fn cleanup_worker<G: GitOps, H: HerdrOps>(
    name: &str,
    repo_path: &Path,
    git: &G,
    herdr: &H,
) -> Result<(), (i32, String)> {
    cleanup_worker_in(name, repo_path, git, herdr, None)
}

#[cfg(test)]
pub(crate) mod metrics {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    use std::process::Output;

    /// Serialises the tests below against process-global env and temp dirs. A
    /// panicking test must not wedge the rest, so poisoning is ignored (same
    /// rule as [`crate::test_env::lock_env`]).
    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A test that panics while holding [`TEST_LOCK`] must not cascade into
    /// every later test: acquiring the lock again still has to succeed.
    #[test]
    fn test_cleanup_lock_recovers_from_a_panicking_holder() {
        let panicked = std::thread::spawn(|| {
            let _lock = test_lock();
            panic!("deliberate panic while holding TEST_LOCK (poison recovery test)");
        })
        .join();
        assert!(panicked.is_err(), "holder thread should have panicked");

        let _lock = test_lock();
    }

    /// Restores an environment variable when dropped, including on panic.
    struct TestEnvVarGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl TestEnvVarGuard {
        fn set(key: &'static str, val: impl AsRef<std::ffi::OsStr>) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            Self { key, prev }
        }
    }

    impl Drop for TestEnvVarGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(val) => std::env::set_var(self.key, val),
                None => std::env::remove_var(self.key),
            }
        }
    }

    struct MockGit {
        worktree_remove_ok: bool,
    }

    impl GitOps for MockGit {
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
            let code = if self.worktree_remove_ok { 0 } else { 1 };
            Ok(Output {
                status: ExitStatusExt::from_raw(code),
                stdout: vec![],
                stderr: if code == 0 {
                    vec![]
                } else {
                    b"failed".to_vec()
                },
            })
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

    const LIST_COS: &str = r#"{"result":{"agents":[{"name":"cos"}]}}"#;

    struct MockHerdr {
        agent_get_response: String,
        agent_list_response: String,
    }

    impl HerdrOps for MockHerdr {
        fn run(&self, _args: &[&str]) -> std::io::Result<Output> {
            Ok(Output {
                status: ExitStatusExt::from_raw(0),
                stdout: vec![],
                stderr: vec![],
            })
        }
        fn agent_list(&self) -> std::io::Result<String> {
            Ok(self.agent_list_response.clone())
        }
        fn agent_get(&self, _name: &str) -> std::io::Result<(bool, String)> {
            Ok((true, self.agent_get_response.clone()))
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
            _args: &[&str],
        ) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn pane_split(
            &self,
            _cmd: &str,
            _orientation: &str,
            _envs: &[(&str, &str)],
        ) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn pane_close(&self, _pane_id: &str) -> std::io::Result<Output> {
            Ok(Output {
                status: ExitStatusExt::from_raw(0),
                stdout: vec![],
                stderr: vec![],
            })
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
        fn agent_wait_idle(&self, _name: &str, _timeout_ms: u32) -> std::io::Result<Output> {
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
        fn agent_send_keys(&self, _name: &str, _keys: &str) -> std::io::Result<Output> {
            unimplemented!()
        }
    }

    #[test]
    fn test_cleanup_metrics_happy_path() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let tmp =
            std::env::temp_dir().join(format!("cleanup-metrics-happy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let tmp = std::fs::canonicalize(&tmp).unwrap();
        let runs_dir = tmp.join("runs");
        let repo = tmp.join("repo");
        let wt = tmp.join(".wt").join("tieude-clean1");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        let _env_owner = crate::test_env::TestEnvVarGuard::remove("QUAQUA_OWNER");
        let _env_home = TestEnvVarGuard::set("HOME", &tmp);
        let _env_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", &runs_dir);
        let _env_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _env_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");

        // Fixture opencode DB
        let state_dir = runs_dir
            .join(".opencode-state")
            .join("tieude-clean1")
            .join("opencode");
        std::fs::create_dir_all(&state_dir).unwrap();
        let db_path = state_dir.join("opencode.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_v2 (
                id TEXT PRIMARY KEY,
                directory TEXT NOT NULL,
                cost REAL DEFAULT 0 NOT NULL,
                tokens_input INTEGER DEFAULT 0 NOT NULL,
                tokens_output INTEGER DEFAULT 0 NOT NULL,
                tokens_reasoning INTEGER DEFAULT 0 NOT NULL,
                tokens_cache_read INTEGER DEFAULT 0 NOT NULL,
                tokens_cache_write INTEGER DEFAULT 0 NOT NULL,
                model TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_v2 VALUES ('ses-1', ?1, 0.0458, 94574, 16991, 0, 3754496, 0, '{\"id\":\"deepseek-v4.1-flash\"}', 1000, 2000);",
            rusqlite::params![wt.to_str().unwrap()],
        ).unwrap();

        // Metadata
        WorkerMetadata::save_owner(&runs_dir, "tieude-clean1", "cos").unwrap();
        WorkerMetadata::save_pipeline(&runs_dir, "tieude-clean1", "task-p74").unwrap();

        let herdr = MockHerdr {
            agent_list_response: LIST_COS.to_string(),
            agent_get_response: format!(
                r#"{{"result":{{"agent":{{"agent":"opencode","pane_id":"%1","cwd":"{}","agent_session":{{"value":"ses-1"}}}}}}}}"#,
                wt.display()
            ),
        };
        let git = MockGit {
            worktree_remove_ok: true,
        };

        cleanup_worker_in("tieude-clean1", &repo, &git, &herdr, Some(&runs_dir)).unwrap();

        // Verify telemetry.jsonl
        let tele_path = runs_dir.join("telemetry.jsonl");
        assert!(tele_path.is_file());
        let content = std::fs::read_to_string(&tele_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let row: WorkerTelemetryRow = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(row.rec, "worker");
        assert_eq!(row.name, "tieude-clean1");
        assert_eq!(row.owner.as_deref(), Some("cos"));
        assert_eq!(row.pipeline.as_deref(), Some("task-p74"));
        assert_eq!(row.harness.as_deref(), Some("opencode"));
        assert_eq!(row.spend_src, "opencode-db");
        assert_eq!(row.model.as_deref(), Some("deepseek-v4.1-flash"));
        assert!((row.usd.unwrap() - 0.0458).abs() < 1e-6);
        let tok = row.tokens.unwrap();
        assert_eq!(tok.in_tokens, 94574);
        assert_eq!(tok.out_tokens, 16991);
        assert_eq!(tok.cache_read, 3754496);
        assert_eq!(row.prompts, None);
        assert_eq!(row.repo.as_deref(), Some("~/repo"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cleanup_refuses_when_not_found_but_still_listed() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let tmp = std::env::temp_dir().join(format!("cleanup-listed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let runs_dir = tmp.join("runs");
        let repo = tmp.join("repo");
        let wt = tmp.join(".wt").join("worker-listed");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt).unwrap();

        let _env_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", &runs_dir);
        let _env_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _env_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");

        let herdr = MockHerdr {
            agent_list_response: r#"{"result":{"agents":[{"name":"worker-listed"}]}}"#.to_string(),
            agent_get_response: r#"{"error":{"code":"agent_not_found"}}"#.to_string(),
        };
        let git = MockGit {
            worktree_remove_ok: true,
        };

        let err =
            cleanup_worker_in("worker-listed", &repo, &git, &herdr, Some(&runs_dir)).unwrap_err();
        assert_eq!(err.0, 1);
        assert!(err.1.contains("cannot determine"), "{}", err.1);
        assert!(
            wt.is_dir(),
            "worktree must survive an unknown-liveness refusal"
        );
        assert!(!runs_dir.join("telemetry.jsonl").exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cleanup_repo_inferred_from_spawn_record() {
        let tmp = std::env::temp_dir().join(format!("cleanup-infer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let runs_dir = tmp.join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        assert_eq!(resolve_cleanup_repo("w-infer", None, &runs_dir), Ok(None));
        WorkerMetadata::save_repo(&runs_dir, "w-infer", &tmp.join("repo")).unwrap();
        assert_eq!(
            resolve_cleanup_repo("w-infer", None, &runs_dir),
            Ok(Some(tmp.join("repo")))
        );
        assert_eq!(
            resolve_cleanup_repo("w-infer", Some(&tmp.join("repo")), &runs_dir),
            Ok(Some(tmp.join("repo")))
        );
        // The record wins over an explicit repo for any other repo (#170 F1).
        let err = resolve_cleanup_repo("w-infer", Some(Path::new("/x/explicit")), &runs_dir)
            .expect_err("an explicit repo that disagrees with the record must be refused");
        assert!(
            err.contains("/x/explicit") && err.contains(&tmp.join("repo").display().to_string()),
            "the refusal must name both repos: {err}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Real-git sandbox with two repos under different parents, so the derived
    /// worktree path `<repo>.parent()/.wt/<name>` differs between them: the
    /// owner repo's is `<tmp>/.wt/<name>`, the foreign repo's would be
    /// `<tmp>/other/.wt/<name>`. Returns (tmp, runs_dir, owner, foreign).
    fn worktree_sandbox(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let tmp = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("cleanup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let runs_dir = tmp.join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        let owner = tmp.join("a");
        let foreign = tmp.join("other").join("b");
        crate::test_env::init_repo(&owner);
        crate::test_env::init_repo(&foreign);
        (tmp, runs_dir, owner, foreign)
    }

    fn cleanup_env(runs_dir: &Path) -> Vec<crate::test_env::TestEnvVarGuard> {
        use crate::test_env::TestEnvVarGuard as G;
        vec![
            G::remove("QUAQUA_OWNER"),
            G::set("QUAQUA_RUNS_DIR", runs_dir),
            G::set("GRID_SKIP", "1"),
            G::set("QUAQUA_TEST", "1"),
        ]
    }

    fn gone_herdr() -> MockHerdr {
        MockHerdr {
            agent_list_response: LIST_COS.to_string(),
            agent_get_response: r#"{"error":{"code":"agent_not_found"}}"#.to_string(),
        }
    }

    /// #170 F1: a repo with no trace of the worker must not be reported as
    /// "cleaned", and must not consume the metadata that finds the real
    /// worktree.
    #[test]
    fn test_cleanup_foreign_repo_refused_keeps_metadata() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let (tmp, runs_dir, owner, foreign) = worktree_sandbox("foreign");
        let _env = cleanup_env(&runs_dir);
        let git = crate::git::Git::new();
        let wt = tmp.join(".wt").join("w1");
        let add = git.worktree_add(&owner, "quaqua/w1", &wt, None).unwrap();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        WorkerMetadata::save_owner(&runs_dir, "w1", "cos").unwrap();

        let res = cleanup_worker_in("w1", &foreign, &git, &gone_herdr(), Some(&runs_dir));
        res.expect_err("cleanup against a repo with no trace of w1 must be refused");

        assert!(wt.is_dir(), "the real worktree must survive");
        assert!(
            runs_dir.join("w1.owner").exists(),
            "metadata must survive a refused cleanup"
        );
        assert!(
            !runs_dir.join("telemetry.jsonl").exists(),
            "a refused cleanup must not record telemetry"
        );
        assert!(
            git.worktree_list_porcelain(&owner)
                .unwrap()
                .iter()
                .any(|p| Path::new(p) == wt),
            "the owner repo must still register the worktree"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A dead orphan (no agent, no worktree, no branch) whose spawn record
    /// names this very repo is cleaned, so its metadata is not left for hand
    /// removal; a second run changes nothing more.
    #[test]
    fn test_cleanup_dead_orphan_with_recorded_repo_is_cleaned() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let (tmp, runs_dir, owner, _foreign) = worktree_sandbox("orphan");
        let _env = cleanup_env(&runs_dir);
        let git = crate::git::Git::new();
        WorkerMetadata::save_owner(&runs_dir, "w-orph", "cos").unwrap();
        WorkerMetadata::save_repo(&runs_dir, "w-orph", &owner).unwrap();

        cleanup_worker_in("w-orph", &owner, &git, &gone_herdr(), Some(&runs_dir))
            .expect("a dead orphan of this repo must be cleaned");
        assert!(!runs_dir.join("w-orph.owner").exists());
        assert!(!runs_dir.join("w-orph.repo").exists());
        assert!(
            !runs_dir.join("telemetry.jsonl").exists(),
            "an orphan never ran in a worktree: no telemetry row"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A live agent is never an orphan, even with no worktree and no branch.
    #[test]
    fn test_cleanup_live_agent_without_worktree_is_refused() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let (tmp, runs_dir, owner, _foreign) = worktree_sandbox("orphan-live");
        let _env = cleanup_env(&runs_dir);
        WorkerMetadata::save_owner(&runs_dir, "w-live", "cos").unwrap();
        WorkerMetadata::save_repo(&runs_dir, "w-live", &owner).unwrap();
        let herdr = MockHerdr {
            agent_list_response: r#"{"result":{"agents":[{"name":"w-live"}]}}"#.to_string(),
            agent_get_response:
                r#"{"result":{"agent":{"name":"w-live","pane_id":"w1:p1","workspace_id":"w1"}}}"#
                    .to_string(),
        };

        cleanup_worker_in(
            "w-live",
            &owner,
            &crate::git::Git::new(),
            &herdr,
            Some(&runs_dir),
        )
        .expect_err("a live agent must not be cleaned as an orphan");
        assert!(runs_dir.join("w-live.owner").exists());
        assert!(runs_dir.join("w-live.repo").exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// #170 F2: a Lead is cleaned by `quaqua lead cleanup <slug>`, which also
    /// closes the team workspace and returns the project to cos.
    #[test]
    fn test_cleanup_refuses_lead_name() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let (tmp, runs_dir, owner, _foreign) = worktree_sandbox("lead");
        let _env = cleanup_env(&runs_dir);
        WorkerMetadata::save_owner(&runs_dir, "mu-lead", "cos").unwrap();

        let res = cleanup_worker_in(
            "mu-lead",
            &owner,
            &crate::git::Git::new(),
            &gone_herdr(),
            Some(&runs_dir),
        );
        let err = res
            .expect_err("a Lead must not go through worker cleanup")
            .1;
        assert!(err.contains("quaqua lead cleanup mu"), "{err}");
        assert!(
            runs_dir.join("mu-lead.owner").exists(),
            "the Lead's owner record must survive"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// #170 F3: a worktree deleted by hand leaves git's registration behind;
    /// cleanup must drop it, or a same-name spawn dies on
    /// "missing but already registered worktree".
    #[test]
    fn test_cleanup_gone_worktree_drops_registration() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let (tmp, runs_dir, owner, _foreign) = worktree_sandbox("gone");
        let _env = cleanup_env(&runs_dir);
        let git = crate::git::Git::new();
        let wt = tmp.join(".wt").join("w-gone");
        let add = git
            .worktree_add(&owner, "quaqua/w-gone", &wt, None)
            .unwrap();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        std::fs::remove_dir_all(&wt).unwrap();
        WorkerMetadata::save_owner(&runs_dir, "w-gone", "cos").unwrap();

        cleanup_worker_in("w-gone", &owner, &git, &gone_herdr(), Some(&runs_dir)).unwrap();

        assert!(!runs_dir.join("w-gone.owner").exists());
        assert!(
            !git.worktree_list_porcelain(&owner)
                .unwrap()
                .iter()
                .any(|p| Path::new(p) == wt),
            "git must no longer register the deleted worktree"
        );
        // The same name can be spawned again.
        let out = git
            .worktree_add_existing(&owner, "quaqua/w-gone", &wt)
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cleanup_metrics_all_readers_failing() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let tmp = std::env::temp_dir().join(format!("cleanup-metrics-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let runs_dir = tmp.join("runs");
        let repo = tmp.join("repo");
        let wt = tmp.join(".wt").join("worker-unknown");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt).unwrap();

        let _env_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", &runs_dir);
        let _env_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _env_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");

        let herdr = MockHerdr {
            agent_list_response: LIST_COS.to_string(),
            agent_get_response: r#"{"error":{"code":"agent_not_found"}}"#.to_string(),
        };
        let git = MockGit {
            worktree_remove_ok: true,
        };

        // Even with no session DB, no owner, agent not found -> cleanup must succeed!
        cleanup_worker_in("worker-unknown", &repo, &git, &herdr, Some(&runs_dir)).unwrap();

        let tele_path = runs_dir.join("telemetry.jsonl");
        assert!(tele_path.is_file());
        let content = std::fs::read_to_string(&tele_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let row: WorkerTelemetryRow = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(row.rec, "worker");
        assert_eq!(row.name, "worker-unknown");
        assert_eq!(row.usd, None);
        assert_eq!(row.tokens, None);
        assert!(!row.spend_src.is_empty());
        assert_eq!(row.prompts, None);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cleanup_metrics_second_lifecycle_excludes_first() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let tmp =
            std::env::temp_dir().join(format!("cleanup-metrics-reuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let runs_dir = tmp.join("runs");
        let repo = tmp.join("repo");
        let wt = tmp.join(".wt").join("tieude-reused");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt).unwrap();

        let _env_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", &runs_dir);
        let _env_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _env_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");

        // Seed telemetry.jsonl with previous lifecycle for tieude-reused at ts=2026-10-21T10:00:00Z
        let tele_path = runs_dir.join("telemetry.jsonl");
        let prev_line = "{\"ts\":\"2026-10-21T10:00:00Z\",\"rec\":\"worker\",\"name\":\"tieude-reused\",\"usd\":0.5}\n";
        std::fs::write(&tele_path, prev_line).unwrap();

        let fence_ms =
            crate::worker::prompt::cost::parse_iso_timestamp_ms("2026-10-21T10:00:00Z").unwrap();

        // Fixture opencode DB with 2 sessions:
        // Session 1: before fence_ms
        // Session 2: after fence_ms
        let state_dir = runs_dir
            .join(".opencode-state")
            .join("tieude-reused")
            .join("opencode");
        std::fs::create_dir_all(&state_dir).unwrap();
        let db_path = state_dir.join("opencode.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_v2 (
                id TEXT PRIMARY KEY,
                directory TEXT NOT NULL,
                cost REAL DEFAULT 0 NOT NULL,
                tokens_input INTEGER DEFAULT 0 NOT NULL,
                tokens_output INTEGER DEFAULT 0 NOT NULL,
                tokens_reasoning INTEGER DEFAULT 0 NOT NULL,
                tokens_cache_read INTEGER DEFAULT 0 NOT NULL,
                tokens_cache_write INTEGER DEFAULT 0 NOT NULL,
                model TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL
            );",
        )
        .unwrap();
        // Session 1 (before fence)
        conn.execute(
            "INSERT INTO session_v2 VALUES ('ses-old', ?1, 0.50, 5000, 500, 0, 10000, 0, 'deepseek-v4.1-flash', ?2, ?2);",
            rusqlite::params![wt.to_str().unwrap(), (fence_ms - 10000) as i64],
        ).unwrap();
        // Session 2 (current lifecycle)
        conn.execute(
            "INSERT INTO session_v2 VALUES ('ses-new', ?1, 0.08, 1000, 200, 0, 2000, 0, 'deepseek-v4.1-flash', ?2, ?2);",
            rusqlite::params![wt.to_str().unwrap(), (fence_ms + 10000) as i64],
        ).unwrap();

        let herdr = MockHerdr {
            agent_list_response: LIST_COS.to_string(),
            agent_get_response: format!(
                r#"{{"result":{{"agent":{{"agent":"opencode","pane_id":"%1","cwd":"{}","agent_session":{{"value":"ses-new"}}}}}}}}"#,
                wt.display()
            ),
        };
        let git = MockGit {
            worktree_remove_ok: true,
        };

        cleanup_worker_in("tieude-reused", &repo, &git, &herdr, Some(&runs_dir)).unwrap();

        let content = std::fs::read_to_string(&tele_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let row2: WorkerTelemetryRow = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(row2.rec, "worker");
        assert_eq!(row2.name, "tieude-reused");
        // Must exclude session 1 (0.50) and only record session 2 (0.08)!
        assert!((row2.usd.unwrap() - 0.08).abs() < 1e-6);
        let tok = row2.tokens.unwrap();
        assert_eq!(tok.in_tokens, 1000);
        assert_eq!(tok.out_tokens, 200);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cleanup_failure_retry_produces_single_row() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let tmp = std::env::temp_dir().join(format!("cleanup-fail-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let runs_dir = tmp.join("runs");
        let repo = tmp.join("repo");
        let wt = tmp.join(".wt").join("tieude-retry");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt).unwrap();

        let _env_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", &runs_dir);
        let _env_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _env_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");

        let herdr = MockHerdr {
            agent_list_response: LIST_COS.to_string(),
            agent_get_response: r#"{"result":{"agent":{"agent":"opencode","pane_id":"%1","cwd":"","agent_session":{"value":"ses-1"}}}}"#.to_string(),
        };

        // 1. Initial attempt fails at worktree_remove
        let failing_git = MockGit {
            worktree_remove_ok: false,
        };
        let err = cleanup_worker_in("tieude-retry", &repo, &failing_git, &herdr, Some(&runs_dir));
        assert!(err.is_err());

        let tele_path = runs_dir.join("telemetry.jsonl");
        assert!(
            !tele_path.exists(),
            "no telemetry row must be written when cleanup fails"
        );

        // 2. Retry succeeds
        let success_git = MockGit {
            worktree_remove_ok: true,
        };
        let res = cleanup_worker_in("tieude-retry", &repo, &success_git, &herdr, Some(&runs_dir));
        assert!(res.is_ok());

        assert!(tele_path.is_file());
        let content = std::fs::read_to_string(&tele_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "retry after failure must produce exactly 1 row"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_stop_row_then_cleanup_counts_worker_once() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let tmp = std::env::temp_dir().join(format!("cleanup-stop-row-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let runs_dir = tmp.join("runs");
        let repo = tmp.join("repo");
        let wt = tmp.join(".wt").join("agy-stopped");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt).unwrap();

        let _env_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", &runs_dir);
        let _env_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _env_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");

        let herdr = MockHerdr {
            agent_list_response: LIST_COS.to_string(),
            agent_get_response: r#"{"result":{"agent":{"agent":"agy","pane_id":"%1","cwd":""}}}"#
                .to_string(),
        };

        // The watchdog stop (exit 4) writes the row; a repeat stop adds nothing.
        let stop_line = "{\"ts\":\"2026-11-12T15:00:00Z\",\"rec\":\"worker\",\"name\":\"agy-stopped\",\"exit\":4}\n";
        let tele_path = runs_dir.join("telemetry.jsonl");
        std::fs::write(&tele_path, stop_line).unwrap();
        record_stop_telemetry("agy-stopped", &repo, &herdr, &runs_dir);
        assert_eq!(
            std::fs::read_to_string(&tele_path).unwrap().lines().count(),
            1
        );

        let git = MockGit {
            worktree_remove_ok: true,
        };
        cleanup_worker_in("agy-stopped", &repo, &git, &herdr, Some(&runs_dir)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&tele_path).unwrap().lines().count(),
            1,
            "cleanup after a stop row must not count the worker twice"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cleanup_metrics_journal_fields() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let tmp = std::env::temp_dir().join(format!("cleanup-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let tmp = std::fs::canonicalize(&tmp).unwrap();
        let runs_dir = tmp.join("runs");
        let repo = tmp.join("repo");
        let wt = tmp.join(".wt").join("tieude-journal");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt).unwrap();

        let _env_home = TestEnvVarGuard::set("HOME", &tmp);
        let _env_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", &runs_dir);
        let _env_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _env_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let db_path = runs_dir.join("quaqua.db");
        let mut journal = crate::engine::journal_sqlite::SqliteJournal::open(&db_path).unwrap();

        let mut state = crate::worker::prompt::workflow::SingleWorkerState::new(
            crate::engine::child::SingleWorkerInput::new(
                "tieude-journal",
                "tieude",
                repo.to_string_lossy(),
                "brief",
                1800,
                Some(2.50),
            ),
        );
        state.harness = Some("opencode".to_string());
        let state_json = serde_json::to_string(&state).unwrap();
        journal
            .append_snapshot(
                "worker:tieude-journal#1",
                &crate::engine::journal::SnapshotRecord::new(1, state_json),
            )
            .unwrap();

        let now_ms = crate::util::now_sec() * 1000;
        journal
            .append_event(
                "worker:tieude-journal#1",
                &crate::engine::journal::EventRecord::new(
                    1,
                    now_ms.saturating_sub(60_000),
                    "workflow_started".to_string(),
                    "{}".to_string(),
                ),
            )
            .unwrap();
        journal
            .append_event(
                "worker:tieude-journal#1",
                &crate::engine::journal::EventRecord::new(
                    2,
                    now_ms.saturating_sub(10_000),
                    "workflow_completed".to_string(),
                    r#"{"exit":0,"report_path":"/tmp/r.md"}"#.to_string(),
                ),
            )
            .unwrap();

        let herdr = MockHerdr {
            agent_list_response: LIST_COS.to_string(),
            agent_get_response: format!(
                r#"{{"result":{{"agent":{{"agent":"opencode","pane_id":"%1","cwd":"{}","agent_session":{{"value":"ses-j1"}}}}}}}}"#,
                wt.display()
            ),
        };
        let git = MockGit {
            worktree_remove_ok: true,
        };

        cleanup_worker_in("tieude-journal", &repo, &git, &herdr, Some(&runs_dir)).unwrap();

        let tele_path = runs_dir.join("telemetry.jsonl");
        assert!(tele_path.is_file());
        let content = std::fs::read_to_string(&tele_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let row: WorkerTelemetryRow = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(row.prompts, Some(1));
        assert_eq!(row.exit, Some(0));
        assert!(row.wall_sec.unwrap() >= 10);
        assert_eq!(row.kind.as_deref(), Some("tieude"));
        assert_eq!(row.budget_sec, Some(1800));
        assert!((row.usd_budget.unwrap() - 2.50).abs() < 1e-6);
        assert_eq!(row.repo.as_deref(), Some("~/repo"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cleanup_metrics_failing_harness_readers() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let tmp =
            std::env::temp_dir().join(format!("cleanup-failing-readers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let runs_dir = tmp.join("runs");
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        // A live worker owns a worktree; the metrics readers run behind that check.
        for name in ["sude-fail", "tieude-fail", "truonglao-fail"] {
            std::fs::create_dir_all(tmp.join(".wt").join(name)).unwrap();
        }

        let _env_runs = TestEnvVarGuard::set("QUAQUA_RUNS_DIR", &runs_dir);
        let _env_skip = TestEnvVarGuard::set("GRID_SKIP", "1");
        let _env_test = TestEnvVarGuard::set("QUAQUA_TEST", "1");

        let git = MockGit {
            worktree_remove_ok: true,
        };

        // 1. OMP failing reader (no session directory)
        let herdr_omp = MockHerdr {
            agent_list_response: LIST_COS.to_string(),
            agent_get_response: r#"{"result":{"agent":{"agent":"omp","pane_id":"%1","cwd":"/nonexistent/wt/.wt/sude-fail","agent_session":{"value":"ses-omp"}}}}"#.to_string(),
        };
        cleanup_worker_in("sude-fail", &repo, &git, &herdr_omp, Some(&runs_dir)).unwrap();

        // 2. Opencode failing reader (no sqlite db)
        let herdr_open = MockHerdr {
            agent_list_response: LIST_COS.to_string(),
            agent_get_response: r#"{"result":{"agent":{"agent":"opencode","pane_id":"%2","cwd":"/nonexistent/wt/.wt/tieude-fail","agent_session":{"value":"ses-open"}}}}"#.to_string(),
        };
        cleanup_worker_in("tieude-fail", &repo, &git, &herdr_open, Some(&runs_dir)).unwrap();

        // 3. Claude failing reader (no projects transcripts)
        let herdr_claude = MockHerdr {
            agent_list_response: LIST_COS.to_string(),
            agent_get_response: r#"{"result":{"agent":{"agent":"claude","pane_id":"%3","cwd":"/nonexistent/wt/.wt/truonglao-fail","agent_session":{"value":"ses-claude"}}}}"#.to_string(),
        };
        cleanup_worker_in(
            "truonglao-fail",
            &repo,
            &git,
            &herdr_claude,
            Some(&runs_dir),
        )
        .unwrap();

        let tele_path = runs_dir.join("telemetry.jsonl");
        let content = std::fs::read_to_string(&tele_path).unwrap();
        let rows: Vec<WorkerTelemetryRow> = content
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);

        // OMP: usd null, tokens null, spend_src omp-session-not-found
        assert_eq!(rows[0].name, "sude-fail");
        assert_eq!(rows[0].usd, None);
        assert_eq!(rows[0].tokens, None);
        assert_eq!(rows[0].spend_src, "omp-session-not-found");

        // Opencode: usd null, tokens null, spend_src opencode-db-not-found
        assert_eq!(rows[1].name, "tieude-fail");
        assert_eq!(rows[1].usd, None);
        assert_eq!(rows[1].tokens, None);
        assert_eq!(rows[1].spend_src, "opencode-db-not-found");

        // Claude: usd null, tokens null, spend_src claude-tokens-not-found
        assert_eq!(rows[2].name, "truonglao-fail");
        assert_eq!(rows[2].usd, None);
        assert_eq!(rows[2].tokens, None);
        assert_eq!(rows[2].spend_src, "claude-tokens-not-found");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_format_repo_path_boundary() {
        let _lock = test_lock();
        let _env_lock = crate::test_env::lock_env();
        let _env_home = TestEnvVarGuard::set("HOME", "/mock/root/user");

        assert_eq!(
            format_repo_path(Path::new("/mock/root/user/quaqua")),
            "~/quaqua"
        );
        assert_eq!(format_repo_path(Path::new("/mock/root/user")), "~");
        // Must not strip as prefix when path boundary does not match:
        assert_eq!(
            format_repo_path(Path::new("/mock/root/user2/quaqua")),
            "/mock/root/user2/quaqua"
        );
    }
}
