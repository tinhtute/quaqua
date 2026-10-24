//! Real activity implementations for the Quaqua workflow engine (E3).
//!
//! Provides [`SystemActivities`], executing typed side-effecting operations
//! over existing [`Herdr`] and [`Git`] adapters, filesystem reports, and cost checks.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::engine::activity::{
    Activity, ActivityError, ActivityRequest, ActivityResult, CostRequest, CostResult, GitRequest,
    GitResult, GridRequest, HerdrRequest, HerdrResult, ReportFileRequest,
    ReportFileResult, SessionRequest, SessionResult,
};
use crate::git::{Git, GitOps};
use crate::herdr::{Herdr, HerdrOps};
/// Production activities implementation executing real operations against
/// herdr, git, and local filesystem resources.
#[derive(Clone)]
pub struct SystemActivities {
    pub herdr: Herdr,
    pub git: Git,
    pub repo_dir: PathBuf,
}

impl std::fmt::Debug for SystemActivities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemActivities")
            .field("herdr", &self.herdr)
            .field("git", &self.git)
            .field("repo_dir", &self.repo_dir)
            .finish()
    }
}

/// Parsed subset of a herdr `agent get` / `agent wait` payload.
struct AgentSnapshot {
    status: String,
    seq: u64,
    session_kind: String,
    session_value: String,
    harness: String,
    cwd: String,
    err: String,
    hook_active: bool,
}

fn parse_agent_text(name: &str, text: &str) -> AgentSnapshot {
    let mut snap = AgentSnapshot {
        status: "unknown".to_string(),
        seq: 0,
        session_kind: String::new(),
        session_value: String::new(),
        harness: String::new(),
        cwd: String::new(),
        err: String::new(),
        hook_active: false,
    };
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(err) = json.get("error") {
            snap.err = err
                .get("code")
                .and_then(|c| c.as_str())
                .unwrap_or("herdr_error")
                .to_string();
        }
        let agent = json
            .pointer("/result/agent")
            .or_else(|| json.get("result").filter(|r| r.is_object()));
        if let Some(a) = agent {
            if let Some(status) = a
                .get("agent_status")
                .or_else(|| a.get("status"))
                .and_then(|v| v.as_str())
            {
                snap.status = status.to_string();
            }
            snap.seq = a
                .get("state_change_seq")
                .or_else(|| a.get("seq"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            snap.harness = a
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            snap.cwd = a
                .get("cwd")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            snap.hook_active = a
                .get("screen_detection_skipped")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if let Some(session) = a.get("agent_session") {
                snap.session_kind = session
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                snap.session_value = session
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
        }
    } else if text.contains("agent_not_found") {
        snap.err = "agent_not_found".to_string();
    }
    if snap.err == "agent_not_found" || snap.err == "agent_not_running" {
        snap.status = "gone".to_string();
    }
    if snap.status == "unknown" && snap.err.is_empty() && !text.trim().is_empty() {
        // Keep name in the diagnostic path without scraping pane text.
        let _ = name;
    }
    snap
}

fn parse_agent_output(name: &str, out: &std::process::Output) -> AgentSnapshot {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = if stdout.trim().is_empty() {
        stderr.as_ref()
    } else {
        stdout.as_ref()
    };
    let mut snap = parse_agent_text(name, text);
    if !out.status.success() && snap.err.is_empty() {
        let msg = text.trim();
        snap.err = if msg.is_empty() {
            format!("herdr agent wait failed (status {:?})", out.status.code())
        } else {
            msg.to_string()
        };
    }
    snap
}

impl SystemActivities {
    /// Creates a new `SystemActivities` using default Herdr and Git adapters
    /// rooted at the process current working directory.
    pub fn new() -> Self {
        Self::with_repo_dir(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    /// Creates a new `SystemActivities` rooted at the specified repository path.
    pub fn with_repo_dir(repo_dir: impl Into<PathBuf>) -> Self {
        Self {
            herdr: Herdr::new(),
            git: Git::new(),
            repo_dir: repo_dir.into(),
        }
    }

    /// Creates a new `SystemActivities` with explicit adapter instances and working directory.
    pub fn with_adapters(herdr: Herdr, git: Git, repo_dir: impl Into<PathBuf>) -> Self {
        Self {
            herdr,
            git,
            repo_dir: repo_dir.into(),
        }
    }
    fn run_with_etxtbusy_retry<F, T>(f: F) -> std::io::Result<T>
    where
        F: Fn() -> std::io::Result<T>,
    {
        let mut attempts = 0;
        loop {
            match f() {
                Err(e) if e.raw_os_error() == Some(26) && attempts < 10 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                res => return res,
            }
        }
    }

    fn execute_herdr(&self, req: &HerdrRequest) -> Result<ActivityResult, ActivityError> {
        match req {
            HerdrRequest::Unknown => Ok(ActivityResult::Unknown),
            HerdrRequest::Prompt { name, task } => {
                let out =
                    Self::run_with_etxtbusy_retry(|| self.herdr.agent_prompt_nowait(name, task))
                        .map_err(|e| {
                            ActivityError::ExecutionFailed(format!(
                                "herdr agent prompt io error: {e}"
                            ))
                        })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let err = if stderr.trim().is_empty() {
                        stdout
                    } else {
                        stderr
                    };
                    return Err(ActivityError::ExecutionFailed(format!(
                        "herdr agent prompt failed: {err}"
                    )));
                }
                Ok(ActivityResult::Herdr(HerdrResult::Prompted {
                    name: name.clone(),
                }))
            }
            HerdrRequest::SendKeys { name, keys } => {
                let out = Self::run_with_etxtbusy_retry(|| self.herdr.agent_send_keys(name, keys))
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!(
                            "herdr agent send-keys io error: {e}"
                        ))
                    })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let err = if stderr.trim().is_empty() {
                        stdout
                    } else {
                        stderr
                    };
                    return Err(ActivityError::ExecutionFailed(format!(
                        "herdr agent send-keys failed: {err}"
                    )));
                }
                Ok(ActivityResult::Herdr(HerdrResult::KeysSent {
                    name: name.clone(),
                }))
            }
            HerdrRequest::Stop { name } => {
                let (ok, output) = Self::run_with_etxtbusy_retry(|| self.herdr.agent_get(name))
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!("herdr agent get io error: {e}"))
                    })?;

                if output.contains("agent_not_found") {
                    return Err(ActivityError::NotFound(format!(
                        "agent {name} not found: {output}"
                    )));
                }

                if !ok {
                    return Err(ActivityError::ExecutionFailed(format!(
                        "herdr agent get failed for {name}: {output}"
                    )));
                }

                // If output contains a pane_id, close the pane.
                let pane_id = serde_json::from_str::<serde_json::Value>(&output)
                    .ok()
                    .and_then(|v| {
                        v.get("result")
                            .and_then(|r| r.get("agent"))
                            .and_then(|a| a.get("pane_id"))
                            .and_then(|p| p.as_str())
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                    });

                let pane = pane_id.ok_or_else(|| {
                    ActivityError::ExecutionFailed(format!(
                        "cannot stop agent {name}: no valid pane_id found in herdr output: {output}"
                    ))
                })?;
                let close_out = Self::run_with_etxtbusy_retry(|| self.herdr.pane_close(&pane))
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!("herdr pane close io error: {e}"))
                    })?;
                if !close_out.status.success() {
                    let err = String::from_utf8_lossy(&close_out.stderr);
                    return Err(ActivityError::ExecutionFailed(format!(
                        "herdr pane close failed: {err}"
                    )));
                }

                Ok(ActivityResult::Herdr(HerdrResult::Stopped {
                    name: name.clone(),
                }))
            }
            HerdrRequest::Spawn {
                name,
                kind,
                repo,
                approve_kinds,
            } => {
                let base_cfg = crate::config::load_team_config().map_err(|(code, msg)| {
                    ActivityError::ExecutionFailed(format!(
                        "load team config failed (exit {code}): {msg}"
                    ))
                })?;
                let proj_slug = crate::pipeline::resolve_project_slug_from_repo(Path::new(repo));
                let cfg = if let Some(slug) = proj_slug.as_deref() {
                    crate::config::resolve(base_cfg.clone(), Some(slug)).unwrap_or(base_cfg)
                } else {
                    base_cfg
                };

                let kind_info = cfg.kinds.get(kind).ok_or_else(|| {
                    ActivityError::ExecutionFailed(format!(
                        "spawn failed (exit 1): error: unknown kind '{kind}' in team.toml\n"
                    ))
                })?;
                crate::config::check_spawn_policy(kind, kind_info, approve_kinds).map_err(
                    |(code, msg)| {
                        ActivityError::ExecutionFailed(format!("spawn failed (exit {code}): {msg}"))
                    },
                )?;

                let opts = crate::worker::spawn::SpawnOptions {
                    base: None,
                    review: false,
                    name: name.clone(),
                    kind: Some(kind.clone()),
                    purpose: None,
                    repo_path: PathBuf::from(repo),
                    approve_kinds: approve_kinds.clone(),
                    solo: None,
                };

                crate::worker::spawn::spawn_worker(opts, &cfg, &self.git, &self.herdr, crate::layout::SpawnOrigin::Activity).map_err(
                    |(code, msg)| {
                        ActivityError::ExecutionFailed(format!("spawn failed (exit {code}): {msg}"))
                    },
                )?;

                Ok(ActivityResult::Herdr(HerdrResult::Spawned {
                    name: name.clone(),
                }))
            }
            HerdrRequest::Cleanup { name, repo } => {
                crate::worker::cleanup::cleanup_worker(
                    name,
                    Path::new(repo),
                    &self.git,
                    &self.herdr,
                )
                .map_err(|(code, msg)| {
                    ActivityError::ExecutionFailed(format!("cleanup failed (exit {code}): {msg}"))
                })?;

                Ok(ActivityResult::Herdr(HerdrResult::Cleaned {
                    name: name.clone(),
                }))
            }
            HerdrRequest::AgentWait {
                name,
                until,
                timeout_ms,
            } => {
                let until_refs: Vec<&str> = until.iter().map(String::as_str).collect();
                let out = Self::run_with_etxtbusy_retry(|| {
                    self.herdr.agent_wait(name, &until_refs, *timeout_ms)
                })
                .map_err(|e| {
                    ActivityError::ExecutionFailed(format!("herdr agent wait io error: {e}"))
                })?;

                let mut snap = parse_agent_output(name, &out);
                // An unrecognised wait payload (older herdr, stub) is not a
                // state: read the current one with `agent get`.
                if snap.status == "unknown" && snap.err.is_empty() && *timeout_ms > 0 {
                    if let Ok((_ok, text)) = self.herdr.agent_get(name) {
                        let mut got = parse_agent_text(name, &text);
                        got.err = String::new();
                        snap = got;
                    }
                }
                // A wait timeout is an observation, not a failure: read the
                // current state once so idle/stall arithmetic stays accurate.
                if snap.err == "timeout" && *timeout_ms > 0 {
                    if let Ok((_ok, text)) = self.herdr.agent_get(name) {
                        let mut got = parse_agent_text(name, &text);
                        got.err = String::new();
                        snap = got;
                    } else {
                        snap.err = String::new();
                    }
                }
                let lead_waiting = crate::worker::prompt::observer::lead_waiting(
                    &crate::worker::cleanup::default_runs_dir(),
                    name,
                    "",
                );
                Ok(ActivityResult::Herdr(HerdrResult::AgentWait {
                    name: name.clone(),
                    status: snap.status,
                    seq: snap.seq,
                    session_kind: snap.session_kind,
                    session_value: snap.session_value,
                    harness: snap.harness,
                    cwd: snap.cwd,
                    err: snap.err,
                    lead_waiting,
                    hook_active: snap.hook_active,
                }))
            }
        }
    }

    fn execute_git(&self, req: &GitRequest) -> Result<ActivityResult, ActivityError> {
        match req {
            GitRequest::WorktreeAdd { path, branch, base } => {
                let out = self
                    .git
                    .worktree_add(&self.repo_dir, branch, Path::new(path), base.as_deref())
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!("git worktree add io error: {e}"))
                    })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(ActivityError::ExecutionFailed(format!(
                        "git worktree add failed: {stderr}"
                    )));
                }
                Ok(ActivityResult::Git(GitResult::WorktreeAdded {
                    path: path.clone(),
                }))
            }
            GitRequest::WorktreeRemove { path } => {
                let out = self
                    .git
                    .worktree_remove(&self.repo_dir, Path::new(path))
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!("git worktree remove io error: {e}"))
                    })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(ActivityError::ExecutionFailed(format!(
                        "git worktree remove failed: {stderr}"
                    )));
                }
                Ok(ActivityResult::Git(GitResult::WorktreeRemoved {
                    path: path.clone(),
                }))
            }
            GitRequest::BranchCreate { name, base } => {
                let out = self
                    .git
                    .run_in(&self.repo_dir, &["branch", name, base])
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!("git branch io error: {e}"))
                    })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(ActivityError::ExecutionFailed(format!(
                        "git branch create failed: {stderr}"
                    )));
                }
                Ok(ActivityResult::Git(GitResult::BranchCreated {
                    name: name.clone(),
                }))
            }
            GitRequest::Commit { message } => {
                let out = self
                    .git
                    .run_in(&self.repo_dir, &["commit", "-m", message])
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!("git commit io error: {e}"))
                    })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(ActivityError::ExecutionFailed(format!(
                        "git commit failed: {stderr}"
                    )));
                }
                let sha_out = self
                    .git
                    .run_in(&self.repo_dir, &["rev-parse", "HEAD"])
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!("git rev-parse io error: {e}"))
                    })?;
                if !sha_out.status.success() {
                    let stderr = String::from_utf8_lossy(&sha_out.stderr);
                    return Err(ActivityError::ExecutionFailed(format!(
                        "git rev-parse HEAD failed: {stderr}"
                    )));
                }
                let sha = String::from_utf8_lossy(&sha_out.stdout).trim().to_string();
                Ok(ActivityResult::Git(GitResult::Committed { sha }))
            }
            GitRequest::Merge { branch } => {
                let out = self
                    .git
                    .run_in(&self.repo_dir, &["merge", "--no-ff", branch])
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!("git merge io error: {e}"))
                    })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(ActivityError::ExecutionFailed(format!(
                        "git merge failed: {stderr}"
                    )));
                }
                let sha_out = self
                    .git
                    .run_in(&self.repo_dir, &["rev-parse", "HEAD"])
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!("git rev-parse io error: {e}"))
                    })?;
                if !sha_out.status.success() {
                    let stderr = String::from_utf8_lossy(&sha_out.stderr);
                    return Err(ActivityError::ExecutionFailed(format!(
                        "git rev-parse HEAD failed: {stderr}"
                    )));
                }
                let sha = String::from_utf8_lossy(&sha_out.stdout).trim().to_string();
                Ok(ActivityResult::Git(GitResult::Merged { sha }))
            }
            GitRequest::CurrentSha => {
                let out = self
                    .git
                    .run_in(&self.repo_dir, &["rev-parse", "HEAD"])
                    .map_err(|e| {
                        ActivityError::ExecutionFailed(format!("git rev-parse io error: {e}"))
                    })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(ActivityError::ExecutionFailed(format!(
                        "git rev-parse HEAD failed: {stderr}"
                    )));
                }
                let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
                Ok(ActivityResult::Git(GitResult::Sha(sha)))
            }
            GitRequest::DiffHash { path } => {
                let target = Path::new(path);
                if !target.exists() {
                    return Err(ActivityError::NotFound(format!("path not found: {path}")));
                }
                if !target.is_dir() {
                    return Err(ActivityError::ExecutionFailed(format!(
                        "path is not a directory: {path}"
                    )));
                }
                let check = self
                    .git
                    .run_in(target, &["rev-parse", "--is-inside-work-tree"]);
                match check {
                    Ok(out)
                        if out.status.success()
                            && String::from_utf8_lossy(&out.stdout).trim() == "true" =>
                    {
                        let (hash, stat) = self.git.diff_and_status(target);
                        Ok(ActivityResult::Git(GitResult::DiffHash { hash, stat }))
                    }
                    Ok(_) => Err(ActivityError::ExecutionFailed(format!(
                        "path is not a git work tree: {path}"
                    ))),
                    Err(e) => Err(ActivityError::ExecutionFailed(format!(
                        "git rev-parse --is-inside-work-tree io error: {e}"
                    ))),
                }
            }
        }
    }

    fn execute_report_file(
        &self,
        req: &ReportFileRequest,
    ) -> Result<ActivityResult, ActivityError> {
        match req {
            ReportFileRequest::Read { path } => match std::fs::read_to_string(path) {
                Ok(content) => Ok(ActivityResult::ReportFile(ReportFileResult::Content(
                    content,
                ))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ActivityError::NotFound(
                    format!("report file not found: {path}"),
                )),
                Err(e) => Err(ActivityError::ExecutionFailed(format!(
                    "cannot read report file {path}: {e}"
                ))),
            },
            ReportFileRequest::Write { path, content } => {
                let p = Path::new(path);
                if let Some(parent) = p.parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                }
                std::fs::write(p, content).map_err(|e| {
                    ActivityError::ExecutionFailed(format!("cannot write report file {path}: {e}"))
                })?;
                Ok(ActivityResult::ReportFile(ReportFileResult::Written))
            }
            ReportFileRequest::CheckMtime { path } => match std::fs::metadata(path) {
                Ok(m) => {
                    let mtime = m.modified().map_err(|e| {
                        ActivityError::ExecutionFailed(format!("cannot get mtime for {path}: {e}"))
                    })?;
                    let ms = mtime
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    Ok(ActivityResult::ReportFile(ReportFileResult::MtimeMs(ms)))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ActivityError::NotFound(
                    format!("report file not found: {path}"),
                )),
                Err(e) => Err(ActivityError::ExecutionFailed(format!(
                    "cannot inspect report file {path}: {e}"
                ))),
            },
            ReportFileRequest::Exists { path } => {
                let exists = Path::new(path).exists();
                Ok(ActivityResult::ReportFile(ReportFileResult::Exists(exists)))
            }
        }
    }

    fn execute_session(&self, req: &SessionRequest) -> Result<ActivityResult, ActivityError> {
        match req {
            SessionRequest::Mtime {
                harness,
                session_kind,
                session_value,
                cwd,
            } => Ok(ActivityResult::Session(SessionResult::Mtime(
                crate::worker::prompt::cost::session_store_mtime(
                    harness,
                    session_kind,
                    session_value,
                    cwd,
                ),
            ))),
        }
    }

    fn execute_grid(&self, req: &GridRequest) -> Result<ActivityResult, ActivityError> {
        let repo_name = std::env::var("PIPELINE_REPO")
            .ok()
            .map(|r| {
                Path::new(&r)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or(r)
            })
            .or_else(|| {
                self.repo_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            });
        let mut grid_act = crate::grid::reconcile::GridActivity::with_driver(self.herdr.clone());
        if let Some(rn) = repo_name {
            grid_act = grid_act.with_repo(rn);
        }
        grid_act.execute(&ActivityRequest::Grid(req.clone()))
    }

    fn execute_cost(&self, req: &CostRequest) -> Result<ActivityResult, ActivityError> {
        match req {
            CostRequest::CheckBudget { spend, budget } => Ok(ActivityResult::Cost(
                CostResult::BudgetExceeded(*spend > *budget),
            )),
            CostRequest::QuerySpend { session_pattern } => {
                let mut db_candidates = Vec::new();
                if let Ok(env_db) = std::env::var("QUAQUA_COST_DB_PATH") {
                    if !env_db.is_empty() {
                        db_candidates.push(PathBuf::from(env_db));
                    }
                }

                let runs_dir = crate::worker::cleanup::default_runs_dir();
                let state_dir = runs_dir.join(".opencode-state");
                if state_dir.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&state_dir) {
                        for entry in entries.flatten() {
                            let p = entry.path().join("opencode").join("opencode.db");
                            if p.is_file() {
                                db_candidates.push(p);
                            }
                        }
                    }
                }

                if let Ok(home) = std::env::var("HOME") {
                    let home_db = PathBuf::from(home).join(".local/share/opencode/opencode.db");
                    if home_db.is_file() {
                        db_candidates.push(home_db);
                    }
                }

                for db_path in db_candidates {
                    if !db_path.is_file() {
                        continue;
                    }
                    let conn = match rusqlite::Connection::open_with_flags(
                        &db_path,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                    ) {
                        Ok(c) => c,
                        Err(e) => {
                            return Err(ActivityError::ExecutionFailed(format!(
                                "cannot open cost database at {}: {e}",
                                db_path.display()
                            )));
                        }
                    };

                    let query = "SELECT cost FROM session_v2 WHERE id = ?1 OR directory = ?1 OR directory LIKE ?1 OR directory LIKE ('%' || ?1) ORDER BY time_updated DESC LIMIT 1;";
                    let mut stmt = match conn.prepare(query) {
                        Ok(s) => s,
                        Err(e) => {
                            let err_msg = e.to_string();
                            if err_msg.contains("no such table") {
                                continue;
                            }
                            return Err(ActivityError::ExecutionFailed(format!(
                                "prepare cost query failed on {}: {e}",
                                db_path.display()
                            )));
                        }
                    };

                    let cost_res: Result<f64, rusqlite::Error> =
                        stmt.query_row(rusqlite::params![session_pattern], |row| row.get(0));

                    match cost_res {
                        Ok(cost) => return Ok(ActivityResult::Cost(CostResult::Spend(cost))),
                        Err(rusqlite::Error::QueryReturnedNoRows) => continue,
                        Err(e) => {
                            return Err(ActivityError::ExecutionFailed(format!(
                                "query cost failed on {}: {e}",
                                db_path.display()
                            )));
                        }
                    }
                }

                Ok(ActivityResult::Cost(CostResult::Spend(0.0)))
            }
        }
    }
}

impl Default for SystemActivities {
    fn default() -> Self {
        Self::new()
    }
}

impl Activity for SystemActivities {
    fn execute(&self, req: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        match req {
            ActivityRequest::Herdr(r) => self.execute_herdr(r),
            ActivityRequest::Git(r) => self.execute_git(r),
            ActivityRequest::Grid(r) => self.execute_grid(r),
            ActivityRequest::ReportFile(r) => self.execute_report_file(r),
            ActivityRequest::Cost(r) => self.execute_cost(r),
            ActivityRequest::Session(r) => self.execute_session(r),
            ActivityRequest::Unknown => Ok(ActivityResult::Unknown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::activity::GridResult;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::sync::LazyLock;
    use std::sync::Mutex;
    use std::sync::MutexGuard;

    static HERDR_TEST_MUTEX: Mutex<()> = Mutex::new(());

    /// One lock order for the whole test binary: [`crate::test_env::lock_env`]
    /// first, then [`HERDR_TEST_MUTEX`]. A test that takes the herdr mutex and
    /// then waits for the env lock deadlocks against every herdr test that
    /// already holds the env lock (`--lib`, 16 threads: the cost fixture test
    /// against `with_herdr_stub`).
    fn lock_herdr_test() -> (MutexGuard<'static, ()>, MutexGuard<'static, ()>) {
        let env = crate::test_env::lock_env();
        let herdr = HERDR_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (env, herdr)
    }

    struct EnvVarGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, val: impl AsRef<std::ffi::OsStr>) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(val) => std::env::set_var(self.key, val),
                None => std::env::remove_var(self.key),
            }
        }
    }

    static STUB_BIN: LazyLock<PathBuf> = LazyLock::new(|| {
        let pid = std::process::id();
        let temp_dir = std::env::temp_dir().join(format!("quaqua-herdr-shared-{}", pid));
        let _ = fs::create_dir_all(&temp_dir);
        let stub_bin = temp_dir.join("herdr_stub.sh");
        let log_file = std::env::temp_dir().join(format!("quaqua-herdr-stub-{}.log", pid));

        let script = format!(
            r#"#!/bin/sh
LOG="{}"
case "$1" in
  agent)
    case "$2" in
      prompt)
        if [ "$3" = "e3-stub-agent" ]; then
          echo "prompted:$4" >> "$LOG"
          exit 0
        else
          echo "unknown agent $3" >&2
          exit 1
        fi
        ;;
      send-keys)
        if [ "$3" = "e3-stub-agent" ]; then
          echo "send-keys:$4" >> "$LOG"
          exit 0
        else
          echo "unknown agent $3" >&2
          exit 1
        fi
        ;;
      read)
        if [ "$3" = "e3-stub-agent" ]; then
          echo "stub output line 1"
          echo "stub output line 2"
          exit 0
        else
          echo "agent_not_found" >&2
          exit 1
        fi
        ;;
      get)
        if [ "$3" = "e3-stub-agent" ] || [ "$3" = "sude-unit-act" ]; then
          echo '{{"result": {{"agent": {{"name": "'"$3"'", "pane_id": "e3-pane-42"}}}}}}'
          exit 0
        elif [ "$3" = "e3-info-agent" ]; then
          echo '{{"result": {{"agent": {{"name": "e3-info-agent", "pane_id": "e3-pane-42", "agent_status": "working", "screen_detection_skipped": true, "agent": "omp"}}}}}}'
          exit 0
        elif [ "$3" = "tieude-stub-agent" ]; then
          echo '{{"result": {{"agent": {{"name": "tieude-stub-agent", "pane_id": "e3-pane-43", "agent_status": "idle"}}}}}}'
          exit 0
        elif [ "$3" = "e3-nopane-agent" ]; then
          exit 0
        elif [ "$3" = "e3-crash-agent" ]; then
          echo '{{"error": {{"code": "daemon_unreachable", "message": "connection refused"}}}}' >&2
          exit 1
        else
          echo '{{"error": {{"code": "agent_not_found"}}}}' >&2
          exit 1
        fi
        ;;
      start)
        echo "agent-start:$3:$4" >> "$LOG"
        exit 0
        ;;
      *)
        echo "unknown agent command $2" >&2
        exit 1
        ;;
    esac
    ;;
  pane)
    case "$2" in
      close)
        if [ "$3" = "e3-pane-42" ] || [ "$3" = "e3-pane-split" ]; then
          echo "closed-pane:$3" >> "$LOG"
          exit 0
        else
          echo "unknown pane $3" >&2
          exit 1
        fi
        ;;
      split)
        echo '{{"result": {{"pane": {{"pane_id": "e3-pane-split"}}}}}}'
        exit 0
        ;;
      report-metadata)
        echo "report-metadata:$3:$4" >> "$LOG"
        exit 0
        ;;
      send-keys)
        exit 0
        ;;
      wait-output)
        exit 0
        ;;
      *)
        echo "unknown pane command $2" >&2
        exit 1
        ;;
    esac
    ;;
  api)
    case "$2" in
      snapshot)
        echo '{{"result": {{"tabs": [{{"tab_id": "w1:t1", "title": "quaqua-workers", "panes": ["e3-pane-42"]}}], "panes": [{{"pane_id": "e3-pane-42", "rect": {{"x": 0, "y": 0, "width": 100, "height": 100}}, "tokens": {{"grid_ready": "1", "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua-workers"}}}}], "agents": [{{"name": "e3-stub-agent", "pane_id": "e3-pane-42"}}]}}}}'
        exit 0
        ;;
      *)
        echo "unknown api command $2" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "unknown command $1" >&2
    exit 1
    ;;
esac
"#,
            log_file.to_string_lossy()
        );

        fs::write(&stub_bin, script).expect("write stub script");
        let mut perms = fs::metadata(&stub_bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&stub_bin, perms).expect("chmod +x stub script");
        stub_bin
    });

    fn with_herdr_stub<F: FnOnce(&SystemActivities, &Path)>(f: F) {
        let _locks = lock_herdr_test();

        let stub_bin = &*STUB_BIN;
        let pid = std::process::id();
        let log_file = std::env::temp_dir().join(format!("quaqua-herdr-stub-{}.log", pid));
        let _ = fs::remove_file(&log_file);

        let temp_dir = std::env::temp_dir().join(format!("quaqua-herdr-act-{}", pid));
        let _ = fs::create_dir_all(&temp_dir);
        let runs_temp = temp_dir.join("runs");
        let _ = fs::create_dir_all(&runs_temp);

        // Strictly isolate environment so no live herdr or live runs dir is touched
        let _guard_herdr = EnvVarGuard::set("HERDR_BIN_PATH", stub_bin.to_str().unwrap());
        let _guard_skip = EnvVarGuard::set("GRID_SKIP", "1");
        let _guard_test = EnvVarGuard::set("QUAQUA_TEST", "1");
        let _guard_runs = EnvVarGuard::set("QUAQUA_RUNS_DIR", runs_temp.to_str().unwrap());

        let stub_herdr = Herdr {
            bin: stub_bin.to_string_lossy().to_string(),
        };
        let activities = SystemActivities::with_adapters(stub_herdr, Git::new(), &temp_dir);

        f(&activities, &log_file);

        let _ = fs::remove_file(&log_file);
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_herdr_prompt_via_stub() {
        with_herdr_stub(|activities, log_file| {
            let req = ActivityRequest::Herdr(HerdrRequest::Prompt {
                name: "e3-stub-agent".to_string(),
                task: "execute plan".to_string(),
            });
            let res = activities.execute(&req).expect("prompt must succeed");
            assert_eq!(
                res,
                ActivityResult::Herdr(HerdrResult::Prompted {
                    name: "e3-stub-agent".to_string(),
                })
            );

            let log_content = fs::read_to_string(log_file).unwrap_or_default();
            assert!(log_content.contains("prompted:execute plan"));
        });
    }

    #[test]
    fn test_herdr_send_keys_via_stub() {
        with_herdr_stub(|activities, log_file| {
            let req = ActivityRequest::Herdr(HerdrRequest::SendKeys {
                name: "e3-stub-agent".to_string(),
                keys: "C-c".to_string(),
            });
            let res = activities.execute(&req).expect("send-keys must succeed");
            assert_eq!(
                res,
                ActivityResult::Herdr(HerdrResult::KeysSent {
                    name: "e3-stub-agent".to_string(),
                })
            );

            let log_content = fs::read_to_string(log_file).unwrap_or_default();
            assert!(log_content.contains("send-keys:C-c"));
        });
    }

    #[test]
    fn test_herdr_agent_wait_via_stub() {
        with_herdr_stub(|activities, _log_file| {
            let req = ActivityRequest::Herdr(HerdrRequest::AgentWait {
                name: "e3-info-agent".to_string(),
                until: Vec::new(),
                timeout_ms: 0,
            });
            let res = activities.execute(&req).expect("agent wait must succeed");
            assert_eq!(
                res,
                ActivityResult::Herdr(HerdrResult::AgentWait {
                    name: "e3-info-agent".to_string(),
                    status: "working".to_string(),
                    seq: 0,
                    session_kind: String::new(),
                    session_value: String::new(),
                    harness: "omp".to_string(),
                    cwd: String::new(),
                    err: String::new(),
                    lead_waiting: false,
                    hook_active: true,
                })
            );

            let gone = ActivityRequest::Herdr(HerdrRequest::AgentWait {
                name: "live-agent-nonexistent".to_string(),
                until: Vec::new(),
                timeout_ms: 0,
            });
            let res = activities.execute(&gone).expect("gone is an observation");
            match res {
                ActivityResult::Herdr(HerdrResult::AgentWait { status, err, .. }) => {
                    assert_eq!(status, "gone");
                    assert_eq!(err, "agent_not_found");
                }
                other => panic!("unexpected result: {other:?}"),
            }
        });
    }

    #[test]
    fn test_session_mtime_missing_store_is_none() {
        let activities = SystemActivities::new();
        let req = ActivityRequest::Session(SessionRequest::Mtime {
            harness: "opencode".to_string(),
            session_kind: "id".to_string(),
            session_value: "ses_does_not_exist".to_string(),
            cwd: "/tmp".to_string(),
        });
        // No readable store for this id: the watchdog treats it as budget-only.
        assert_eq!(
            activities.execute(&req).expect("session mtime"),
            ActivityResult::Session(SessionResult::Mtime(None))
        );
    }

    #[test]
    fn test_herdr_stop_agent_via_stub() {
        with_herdr_stub(|activities, log_file| {
            let req = ActivityRequest::Herdr(HerdrRequest::Stop {
                name: "e3-stub-agent".to_string(),
            });
            let res = activities.execute(&req).expect("stop agent must succeed");
            assert_eq!(
                res,
                ActivityResult::Herdr(HerdrResult::Stopped {
                    name: "e3-stub-agent".to_string(),
                })
            );

            let log_content = fs::read_to_string(log_file).unwrap_or_default();
            assert!(log_content.contains("closed-pane:e3-pane-42"));
        });
    }

    #[test]
    fn test_herdr_stop_missing_pane_id_fails() {
        with_herdr_stub(|activities, _log_file| {
            let req = ActivityRequest::Herdr(HerdrRequest::Stop {
                name: "e3-nopane-agent".to_string(),
            });
            let err = activities
                .execute(&req)
                .expect_err("stopping agent without pane_id must fail");
            match err {
                ActivityError::ExecutionFailed(msg) => {
                    assert!(msg.contains("no valid pane_id"));
                }
                other => panic!("expected ExecutionFailed, got: {other:?}"),
            }
        });
    }

    #[test]
    fn test_herdr_unknown_agent_fails() {
        with_herdr_stub(|activities, _log_file| {
            let req = ActivityRequest::Herdr(HerdrRequest::Prompt {
                name: "live-agent-nonexistent".to_string(),
                task: "should fail".to_string(),
            });
            let err = activities
                .execute(&req)
                .expect_err("unknown agent must fail");
            assert!(matches!(err, ActivityError::ExecutionFailed(_)));
        });
    }

    #[test]
    fn test_herdr_stop_nonexistent_returns_not_found() {
        with_herdr_stub(|activities, _log_file| {
            let req = ActivityRequest::Herdr(HerdrRequest::Stop {
                name: "live-agent-nonexistent".to_string(),
            });
            let err = activities
                .execute(&req)
                .expect_err("stopping missing agent must error");
            assert!(matches!(err, ActivityError::NotFound(_)));
        });
    }

    #[test]
    fn test_herdr_stop_generic_failure_returns_execution_failed() {
        with_herdr_stub(|activities, _log_file| {
            let req = ActivityRequest::Herdr(HerdrRequest::Stop {
                name: "e3-crash-agent".to_string(),
            });
            let err = activities
                .execute(&req)
                .expect_err("daemon failure without agent_not_found must return ExecutionFailed");
            match err {
                ActivityError::ExecutionFailed(msg) => {
                    assert!(
                        msg.contains("daemon_unreachable") || msg.contains("connection refused")
                    );
                }
                other => panic!("expected ExecutionFailed, got: {other:?}"),
            }
        });
    }

    #[test]
    fn test_herdr_bin_path_env_var_resolution() {
        let _locks = lock_herdr_test();

        let stub_bin = &*STUB_BIN;
        let pid = std::process::id();
        let log_file = std::env::temp_dir().join(format!("quaqua-herdr-stub-{}.log", pid));
        let _ = fs::remove_file(&log_file);

        let _guard = EnvVarGuard::set("HERDR_BIN_PATH", stub_bin);

        let activities = SystemActivities::new();
        let req = ActivityRequest::Herdr(HerdrRequest::Prompt {
            name: "e3-stub-agent".to_string(),
            task: "verify env path".to_string(),
        });
        let res = activities
            .execute(&req)
            .expect("prompt via HERDR_BIN_PATH must succeed");
        assert_eq!(
            res,
            ActivityResult::Herdr(HerdrResult::Prompted {
                name: "e3-stub-agent".to_string(),
            })
        );
        let log_content = fs::read_to_string(&log_file).unwrap_or_default();
        assert!(log_content.contains("prompted:verify env path"));
        let _ = fs::remove_file(&log_file);
    }

    #[test]
    fn test_report_file_operations() {
        let pid = std::process::id();
        let temp_dir = std::env::temp_dir().join(format!("quaqua-act-report-{}", pid));
        let _ = fs::create_dir_all(&temp_dir);
        let test_file = temp_dir.join("sub").join("report.md");
        let path_str = test_file.to_string_lossy().to_string();
        let activities = SystemActivities::new();

        // 1. Exists returns false
        let res = activities
            .execute(&ActivityRequest::ReportFile(ReportFileRequest::Exists {
                path: path_str.clone(),
            }))
            .expect("exists query");
        assert_eq!(
            res,
            ActivityResult::ReportFile(ReportFileResult::Exists(false))
        );

        // 2. Read returns NotFound
        let err = activities
            .execute(&ActivityRequest::ReportFile(ReportFileRequest::Read {
                path: path_str.clone(),
            }))
            .expect_err("read missing must fail");
        assert!(matches!(err, ActivityError::NotFound(_)));

        // 3. Write creates file and parent dirs
        let res = activities
            .execute(&ActivityRequest::ReportFile(ReportFileRequest::Write {
                path: path_str.clone(),
                content: "# Report Header\nAll green".to_string(),
            }))
            .expect("write report");
        assert_eq!(res, ActivityResult::ReportFile(ReportFileResult::Written));

        // 4. Exists returns true
        let res = activities
            .execute(&ActivityRequest::ReportFile(ReportFileRequest::Exists {
                path: path_str.clone(),
            }))
            .expect("exists query");
        assert_eq!(
            res,
            ActivityResult::ReportFile(ReportFileResult::Exists(true))
        );

        // 5. Read returns content
        let res = activities
            .execute(&ActivityRequest::ReportFile(ReportFileRequest::Read {
                path: path_str.clone(),
            }))
            .expect("read report");
        assert_eq!(
            res,
            ActivityResult::ReportFile(ReportFileResult::Content(
                "# Report Header\nAll green".to_string()
            ))
        );

        // 6. CheckMtime returns timestamp > 0
        let res = activities
            .execute(&ActivityRequest::ReportFile(
                ReportFileRequest::CheckMtime {
                    path: path_str.clone(),
                },
            ))
            .expect("check mtime");
        match res {
            ActivityResult::ReportFile(ReportFileResult::MtimeMs(ms)) => {
                assert!(ms > 0);
            }
            other => panic!("unexpected result: {other:?}"),
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_git_operations() {
        let _locks = lock_herdr_test();

        let pid = std::process::id();
        let temp_dir = std::env::temp_dir().join(format!("quaqua-act-git-{}", pid));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).expect("create git test dir");

        // Initialize a clean test repository
        let run_cmd = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&temp_dir)
                .args(args)
                .output()
                .expect("run git command");
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        run_cmd(&["init"]);
        run_cmd(&["config", "user.email", "test@quaqua.local"]);
        run_cmd(&["config", "user.name", "Quaqua Test"]);

        let dummy_file = temp_dir.join("init.txt");
        fs::write(&dummy_file, "init commit").unwrap();
        run_cmd(&["add", "init.txt"]);
        run_cmd(&["commit", "-m", "init"]);

        let activities = SystemActivities::with_repo_dir(&temp_dir);

        // 1. CurrentSha
        let res = activities
            .execute(&ActivityRequest::Git(GitRequest::CurrentSha))
            .expect("current sha");
        let initial_sha = match res {
            ActivityResult::Git(GitResult::Sha(s)) => {
                assert_eq!(s.len(), 40);
                s
            }
            other => panic!("unexpected: {other:?}"),
        };

        // 2. BranchCreate
        let res = activities
            .execute(&ActivityRequest::Git(GitRequest::BranchCreate {
                name: "feature-branch".to_string(),
                base: "HEAD".to_string(),
            }))
            .expect("branch create");
        assert_eq!(
            res,
            ActivityResult::Git(GitResult::BranchCreated {
                name: "feature-branch".to_string(),
            })
        );

        // 3. Commit
        let second_file = temp_dir.join("second.txt");
        fs::write(&second_file, "second commit").unwrap();
        run_cmd(&["add", "second.txt"]);

        let res = activities
            .execute(&ActivityRequest::Git(GitRequest::Commit {
                message: "second".to_string(),
            }))
            .expect("commit");
        let second_sha = match res {
            ActivityResult::Git(GitResult::Committed { sha }) => {
                assert_eq!(sha.len(), 40);
                assert_ne!(sha, initial_sha);
                sha
            }
            other => panic!("unexpected: {other:?}"),
        };

        // Verify CurrentSha now equals second_sha
        let res = activities
            .execute(&ActivityRequest::Git(GitRequest::CurrentSha))
            .expect("current sha");
        assert_eq!(res, ActivityResult::Git(GitResult::Sha(second_sha)));

        // 4. WorktreeAdd & WorktreeRemove
        let wt_path = std::env::temp_dir().join(format!("quaqua-act-wt-{}", pid));
        let _ = fs::remove_dir_all(&wt_path);

        let res = activities
            .execute(&ActivityRequest::Git(GitRequest::WorktreeAdd {
                path: wt_path.to_string_lossy().to_string(),
                branch: "wt-branch".to_string(),
                base: None,
            }))
            .expect("worktree add");
        assert_eq!(
            res,
            ActivityResult::Git(GitResult::WorktreeAdded {
                path: wt_path.to_string_lossy().to_string(),
            })
        );
        assert!(wt_path.exists());

        let res = activities
            .execute(&ActivityRequest::Git(GitRequest::WorktreeRemove {
                path: wt_path.to_string_lossy().to_string(),
            }))
            .expect("worktree remove");
        assert_eq!(
            res,
            ActivityResult::Git(GitResult::WorktreeRemoved {
                path: wt_path.to_string_lossy().to_string(),
            })
        );
        // 5. Merge
        run_cmd(&["checkout", "-b", "feature-merge"]);
        let merge_file = temp_dir.join("merge-item.txt");
        fs::write(&merge_file, "content to merge").unwrap();
        run_cmd(&["add", "merge-item.txt"]);
        run_cmd(&["commit", "-m", "commit on feature-merge"]);

        run_cmd(&["checkout", "feature-branch"]);

        let res = activities
            .execute(&ActivityRequest::Git(GitRequest::Merge {
                branch: "feature-merge".to_string(),
            }))
            .expect("git merge must succeed");
        let merge_sha = match res {
            ActivityResult::Git(GitResult::Merged { sha }) => {
                assert_eq!(sha.len(), 40);
                sha
            }
            other => panic!("unexpected: {other:?}"),
        };
        assert!(merge_file.exists());
        assert_eq!(
            activities
                .execute(&ActivityRequest::Git(GitRequest::CurrentSha))
                .unwrap(),
            ActivityResult::Git(GitResult::Sha(merge_sha))
        );

        // Merge failure (nonexistent branch)
        let err = activities
            .execute(&ActivityRequest::Git(GitRequest::Merge {
                branch: "nonexistent-branch-404".to_string(),
            }))
            .expect_err("merging invalid branch must fail");
        assert!(matches!(err, ActivityError::ExecutionFailed(_)));

        let _ = fs::remove_dir_all(&temp_dir);
    }
    #[test]
    fn test_git_diff_hash() {
        let _locks = lock_herdr_test();

        let pid = std::process::id();
        let temp_dir = std::env::temp_dir().join(format!("quaqua-act-diffhash-{}", pid));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).expect("create git test dir");

        let run_cmd = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&temp_dir)
                .args(args)
                .output()
                .expect("run git command");
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        let activities = SystemActivities::with_repo_dir(&temp_dir);

        // 1. Before git init: must return ExecutionFailed (not a git work tree)
        let err_pre_init = activities
            .execute(&ActivityRequest::Git(GitRequest::DiffHash {
                path: temp_dir.to_string_lossy().to_string(),
            }))
            .expect_err("must fail on non-git dir");
        assert!(matches!(err_pre_init, ActivityError::ExecutionFailed(_)));

        // 2. Non-existent path: must return NotFound
        let not_found_path = temp_dir.join("nonexistent_sub_dir");
        let err_not_found = activities
            .execute(&ActivityRequest::Git(GitRequest::DiffHash {
                path: not_found_path.to_string_lossy().to_string(),
            }))
            .expect_err("must return NotFound for nonexistent path");
        assert!(matches!(err_not_found, ActivityError::NotFound(_)));

        // 3. Initialize git repo
        run_cmd(&["init"]);
        run_cmd(&["config", "user.email", "test@quaqua.local"]);
        run_cmd(&["config", "user.name", "Quaqua Test"]);

        let dummy_file = temp_dir.join("init.txt");
        fs::write(&dummy_file, "init commit").unwrap();
        run_cmd(&["add", "init.txt"]);
        run_cmd(&["commit", "-m", "init"]);

        // 4. Clean worktree DiffHash
        let res_clean = activities
            .execute(&ActivityRequest::Git(GitRequest::DiffHash {
                path: temp_dir.to_string_lossy().to_string(),
            }))
            .expect("clean diffhash");
        let (clean_hash, clean_stat) = match res_clean {
            ActivityResult::Git(GitResult::DiffHash { hash, stat }) => (hash, stat),
            other => panic!("expected DiffHash, got {other:?}"),
        };
        assert_eq!(clean_stat, "clean");
        assert_eq!(clean_hash.len(), 32);

        // 5. Add uncommitted file -> dirty worktree
        let dirty_file = temp_dir.join("dirty.txt");
        fs::write(&dirty_file, "dirty content").unwrap();

        let res_dirty = activities
            .execute(&ActivityRequest::Git(GitRequest::DiffHash {
                path: temp_dir.to_string_lossy().to_string(),
            }))
            .expect("dirty diffhash");
        let (dirty_hash, dirty_stat) = match res_dirty {
            ActivityResult::Git(GitResult::DiffHash { hash, stat }) => (hash, stat),
            other => panic!("expected DiffHash, got {other:?}"),
        };
        assert_ne!(dirty_hash, clean_hash);
        assert!(dirty_stat.contains("untracked"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_cost_check_budget() {
        let activities = SystemActivities::new();

        let req1 = ActivityRequest::Cost(CostRequest::CheckBudget {
            spend: 5.5,
            budget: 5.0,
        });
        let res1 = activities.execute(&req1).expect("check budget over");
        assert_eq!(res1, ActivityResult::Cost(CostResult::BudgetExceeded(true)));

        let req2 = ActivityRequest::Cost(CostRequest::CheckBudget {
            spend: 3.2,
            budget: 5.0,
        });
        let res2 = activities.execute(&req2).expect("check budget under");
        assert_eq!(
            res2,
            ActivityResult::Cost(CostResult::BudgetExceeded(false))
        );
    }

    #[test]
    fn test_herdr_spawn_invalid_args_fail_with_execution_failed() {
        let activities = SystemActivities::new();
        let req = ActivityRequest::Herdr(HerdrRequest::Spawn {
            name: "invalid-name".to_string(),
            kind: "nonexistent-kind".to_string(),
            repo: ".".to_string(),
            approve_kinds: Vec::new(),
        });
        let err = activities.execute(&req).unwrap_err();
        match err {
            ActivityError::ExecutionFailed(msg) => {
                assert!(
                    msg.contains("spawn failed") || msg.contains("load team config"),
                    "must fail with execution failed containing spawn diagnostics: {msg}"
                );
                assert!(!msg.contains("deferred"), "must not be deferred");
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_herdr_spawn_via_stub() {
        with_herdr_stub(|activities, _log| {
            let pid = std::process::id();
            let base_dir = std::env::temp_dir().join(format!("quaqua-act-spawn-{}", pid));
            let _ = fs::remove_dir_all(&base_dir);
            let temp_repo = base_dir.join("repo");
            let _ = fs::create_dir_all(&temp_repo);

            let repo_str = temp_repo.to_str().unwrap();
            let _ = Command::new("git")
                .args(["init", "-b", "main"])
                .arg(&temp_repo)
                .output();
            let _ = Command::new("git")
                .args(["-C", repo_str, "config", "user.email", "test@quaqua.local"])
                .output();
            let _ = Command::new("git")
                .args(["-C", repo_str, "config", "user.name", "Test User"])
                .output();
            let dummy = temp_repo.join("dummy.txt");
            let _ = fs::write(&dummy, "hello");
            let _ = Command::new("git")
                .args(["-C", repo_str, "add", "dummy.txt"])
                .output();
            let _ = Command::new("git")
                .args(["-C", repo_str, "commit", "-m", "initial"])
                .output();

            // Copy the workspace template team.toml into temp_repo
            let team_path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("templates/workspace/team.toml");
            let _ = fs::copy(&team_path, temp_repo.join("team.toml"));
            let _guard_team = EnvVarGuard::set("QUAQUA_TEAM_TOML", &team_path);
            let _guard_pipe = EnvVarGuard::set("PIPELINE_TASK", "1");
            let req = ActivityRequest::Herdr(HerdrRequest::Spawn {
                name: "sude-spawn-act".to_string(),
                kind: "sude".to_string(),
                repo: temp_repo.to_string_lossy().to_string(),
                approve_kinds: Vec::new(),
            });
            let res = activities
                .execute(&req)
                .expect("spawn executes successfully");
            assert_eq!(
                res,
                ActivityResult::Herdr(HerdrResult::Spawned {
                    name: "sude-spawn-act".to_string(),
                })
            );
            let _ = fs::remove_dir_all(&base_dir);
        });
    }

    #[test]
    fn test_activity_spawn_approve_policy_gate() {
        with_herdr_stub(|activities, _log| {
            let pid = std::process::id();
            let base_dir = std::env::temp_dir().join(format!("quaqua-act-spawn-gate-{}", pid));
            let _ = fs::remove_dir_all(&base_dir);
            let temp_repo = base_dir.join("repo");
            let _ = fs::create_dir_all(&temp_repo);

            let repo_str = temp_repo.to_str().unwrap();
            let _ = Command::new("git")
                .args(["init", "-b", "main"])
                .arg(&temp_repo)
                .output();
            let _ = Command::new("git")
                .args(["-C", repo_str, "config", "user.email", "test@quaqua.local"])
                .output();
            let _ = Command::new("git")
                .args(["-C", repo_str, "config", "user.name", "Test User"])
                .output();
            let dummy = temp_repo.join("dummy.txt");
            let _ = fs::write(&dummy, "hello");
            let _ = Command::new("git")
                .args(["-C", repo_str, "add", "dummy.txt"])
                .output();
            let _ = Command::new("git")
                .args(["-C", repo_str, "commit", "-m", "initial"])
                .output();

            let team_toml_content = r#"
schema_version = 2
[limits]
max_workers_per_orchestrator = 8
[kinds.truonglao]
harness = "claude"
model = "opus-5.5"
effort = "high"
can_review = true
spawn = "approve"
[kinds.sude]
harness = "omp"
model = "google-antigravity/gemini-3.8-flash"
effort = "high"
can_review = false
spawn = "auto"
"#;
            let team_file = base_dir.join("team.toml");
            let _ = fs::write(&team_file, team_toml_content);
            let _guard_team = EnvVarGuard::set("QUAQUA_TEAM_TOML", &team_file);
            let _guard_pipe = EnvVarGuard::set("PIPELINE_TASK", "1");

            // 1. Refused without --approve-kind
            let req_unapproved = ActivityRequest::Herdr(HerdrRequest::Spawn {
                name: "truonglao-spawn-act".to_string(),
                kind: "truonglao".to_string(),
                repo: temp_repo.to_string_lossy().to_string(),
                approve_kinds: Vec::new(),
            });
            let err = activities.execute(&req_unapproved).unwrap_err();
            match err {
                ActivityError::ExecutionFailed(msg) => {
                    assert!(
                        msg.contains("--approve-kind truonglao"),
                        "must refuse unapproved spawn with exact hint: {msg}"
                    );
                }
                other => panic!("expected ExecutionFailed, got: {other:?}"),
            }

            // 2. Allowed with --approve-kind
            let req_approved = ActivityRequest::Herdr(HerdrRequest::Spawn {
                name: "truonglao-spawn-act".to_string(),
                kind: "truonglao".to_string(),
                repo: temp_repo.to_string_lossy().to_string(),
                approve_kinds: vec!["truonglao".to_string()],
            });
            let res = activities
                .execute(&req_approved)
                .expect("approved spawn succeeds");
            assert_eq!(
                res,
                ActivityResult::Herdr(HerdrResult::Spawned {
                    name: "truonglao-spawn-act".to_string(),
                })
            );

            // 3. Auto kind allowed without approve_kinds
            let req_auto = ActivityRequest::Herdr(HerdrRequest::Spawn {
                name: "sude-spawn-act".to_string(),
                kind: "sude".to_string(),
                repo: temp_repo.to_string_lossy().to_string(),
                approve_kinds: Vec::new(),
            });
            let res_auto = activities.execute(&req_auto).expect("auto spawn succeeds");
            assert_eq!(
                res_auto,
                ActivityResult::Herdr(HerdrResult::Spawned {
                    name: "sude-spawn-act".to_string(),
                })
            );

            let _ = fs::remove_dir_all(&base_dir);
        });
    }

    #[test]
    fn test_herdr_cleanup_via_stub() {
        with_herdr_stub(|activities, _log| {
            let pid = std::process::id();
            let base_dir = std::env::temp_dir().join(format!("quaqua-act-clean-{}", pid));
            let _ = fs::remove_dir_all(&base_dir);
            let temp_repo = base_dir.join("repo");
            let _ = fs::create_dir_all(&temp_repo);

            let repo_str = temp_repo.to_str().unwrap();
            let _ = Command::new("git")
                .args(["init", "-b", "main"])
                .arg(&temp_repo)
                .output();
            let _ = Command::new("git")
                .args(["-C", repo_str, "config", "user.email", "test@quaqua.local"])
                .output();
            let _ = Command::new("git")
                .args(["-C", repo_str, "config", "user.name", "Test User"])
                .output();
            let dummy = temp_repo.join("dummy.txt");
            let _ = fs::write(&dummy, "hello");
            let _ = Command::new("git")
                .args(["-C", repo_str, "add", "dummy.txt"])
                .output();
            let _ = Command::new("git")
                .args(["-C", repo_str, "commit", "-m", "initial"])
                .output();

            let wt = base_dir.join(".wt").join("sude-unit-act");
            let wt_out = Command::new("git")
                .args([
                    "-C",
                    repo_str,
                    "worktree",
                    "add",
                    wt.to_str().unwrap(),
                    "-b",
                    "quaqua/sude-unit-act",
                ])
                .output()
                .expect("add worktree");
            assert!(wt_out.status.success());

            let req = ActivityRequest::Herdr(HerdrRequest::Cleanup {
                name: "sude-unit-act".to_string(),
                repo: temp_repo.to_string_lossy().to_string(),
            });
            let res = activities
                .execute(&req)
                .expect("cleanup executes successfully");
            assert_eq!(
                res,
                ActivityResult::Herdr(HerdrResult::Cleaned {
                    name: "sude-unit-act".to_string(),
                })
            );
            let _ = fs::remove_dir_all(&base_dir);
        });
    }

    #[test]
    fn test_grid_reconcile_and_plan_via_stub() {
        with_herdr_stub(|activities, _log| {
            // Plan
            let plan_req = ActivityRequest::Grid(GridRequest::Plan {
                tab: Some("quaqua-workers".to_string()),
            });
            let plan_res = activities.execute(&plan_req).expect("grid plan succeeds");
            if let ActivityResult::Grid(GridResult::PlanJson(json)) = plan_res {
                assert!(json.starts_with('[') && json.ends_with(']'));
            } else {
                panic!("expected PlanJson, got {plan_res:?}");
            }

            // Plan with PIPELINE_REPO set and tab: None (R4 verification: prefer basename)
            {
                let _guard_repo = EnvVarGuard::set("PIPELINE_REPO", "/home/user/work/my-project");
                let plan_none = ActivityRequest::Grid(GridRequest::Plan { tab: None });
                let plan_res_none = activities
                    .execute(&plan_none)
                    .expect("grid plan with PIPELINE_REPO succeeds");
                if let ActivityResult::Grid(GridResult::PlanJson(json)) = plan_res_none {
                    assert!(json.starts_with('[') && json.ends_with(']'));
                } else {
                    panic!("expected PlanJson, got {plan_res_none:?}");
                }
            }
            // Reconcile
            let rec_req = ActivityRequest::Grid(GridRequest::Reconcile {
                tab: Some("quaqua-workers".to_string()),
                only: None,
            });
            let rec_res = activities
                .execute(&rec_req)
                .expect("grid reconcile succeeds");
            if let ActivityResult::Grid(GridResult::Reconciled { changed: _ }) = rec_res {
                // Succeeded without error
            } else {
                panic!("expected Reconciled, got {rec_res:?}");
            }
        });
    }

    #[test]
    fn test_cost_query_spend_fixture_db() {
        let _locks = lock_herdr_test();

        let pid = std::process::id();
        let temp_dir = std::env::temp_dir().join(format!("quaqua-act-cost-db-{}", pid));
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        let empty_path_dir = temp_dir.join("empty_bin");
        let _ = fs::create_dir_all(&empty_path_dir);

        let runs_dir = temp_dir.join("runs");
        let _ = fs::create_dir_all(&runs_dir);

        let home_dir = temp_dir.join("home");
        let _ = fs::create_dir_all(&home_dir);

        let db_path = temp_dir.join("opencode.db");
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open temp sqlite db");
            conn.execute_batch(
                "CREATE TABLE session_v2 (
                    id TEXT PRIMARY KEY,
                    directory TEXT,
                    cost REAL,
                    time_updated INTEGER
                );
                INSERT INTO session_v2 (id, directory, cost, time_updated) VALUES ('sess-alpha', '/tmp/repos/.wt/sude-1', 1.45, 100);
                INSERT INTO session_v2 (id, directory, cost, time_updated) VALUES ('sess-beta', '/tmp/repos/.wt/sude-2', 2.80, 200);",
            )
            .expect("populate session_v2 fixture");
        }

        // Hermetic isolation: empty PATH (proves no external CLI invocation), isolated HOME, isolated RUNS_DIR, and point QUAQUA_COST_DB_PATH
        let _guard_path = EnvVarGuard::set("PATH", empty_path_dir.to_str().unwrap());
        let _guard_home = EnvVarGuard::set("HOME", home_dir.to_str().unwrap());
        let _guard_runs = EnvVarGuard::set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
        let _guard_db = EnvVarGuard::set("QUAQUA_COST_DB_PATH", db_path.to_str().unwrap());

        let start = std::time::Instant::now();
        let activities = SystemActivities::new();

        // 1. Exact session ID match
        let req1 = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "sess-alpha".to_string(),
        });
        let res1 = activities.execute(&req1).expect("query spend alpha");
        assert_eq!(res1, ActivityResult::Cost(CostResult::Spend(1.45)));

        // 2. Directory pattern match
        let req2 = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "%.wt/sude-2".to_string(),
        });
        let res2 = activities.execute(&req2).expect("query spend beta");
        assert_eq!(res2, ActivityResult::Cost(CostResult::Spend(2.80)));

        // 3. Worker substring pattern match
        let req3 = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "sude-1".to_string(),
        });
        let res3 = activities
            .execute(&req3)
            .expect("query spend sude-1 substring");
        assert_eq!(res3, ActivityResult::Cost(CostResult::Spend(1.45)));

        // 4. Non-matching pattern returns Spend(0.0)
        let req4 = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "nonexistent-session".to_string(),
        });
        let res4 = activities.execute(&req4).expect("query spend nonexistent");
        assert_eq!(res4, ActivityResult::Cost(CostResult::Spend(0.0)));

        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "cost query test took too long: {elapsed:?}"
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }
    #[test]
    fn test_all_activity_arms_live_no_deferrals() {
        let _locks = lock_herdr_test();

        let pid = std::process::id();
        let temp_dir = std::env::temp_dir().join(format!("quaqua-act-nodef-{}", pid));
        let _ = fs::create_dir_all(&temp_dir);
        let _guard_home = EnvVarGuard::set("HOME", temp_dir.to_str().unwrap());

        let activities = SystemActivities::new();

        // A deserialize-only tombstone executes as a no-op, never a deferral
        let legacy_req: ActivityRequest =
            serde_json::from_str(r#"{"HerdrEvents":{"PollEvents":null}}"#).expect("legacy request");
        assert_eq!(legacy_req, ActivityRequest::Unknown);
        let legacy_res = activities.execute(&legacy_req).expect("legacy arm live");
        assert_eq!(legacy_res, ActivityResult::Unknown);

        // QuerySpend returns Spend(0.0) rather than deferred error
        let cost_req = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "unknown-pattern".to_string(),
        });
        let cost_res = activities.execute(&cost_req).expect("query spend live");
        assert_eq!(cost_res, ActivityResult::Cost(CostResult::Spend(0.0)));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_activities_suite_completes_with_16_threads() {
        const MARK: &str = "QUAQUA_ACTIVITIES_STRESS_CHILD";
        if std::env::var_os(MARK).is_some() {
            // The child runs this test too; it only has to take its turn.
            return;
        }
        let exe = std::env::current_exe().expect("locate test binary");
        let mut child = {
            // Snapshot the environment while no other test can mutate it: the
            // child inherits this copy at spawn time.
            let _env = crate::test_env::lock_env();
            Command::new(exe)
                .args([
                    "--test-threads=16",
                    "activities",
                    "--skip",
                    "test_activities_suite_completes_with_16_threads",
                ])
                .env(MARK, "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn activities stress child")
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match child.try_wait().expect("poll activities stress child") {
                Some(status) => {
                    assert!(status.success(), "activities suite failed: {status}");
                    return;
                }
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("activities suite deadlocked at 16 threads (still running after 30s)");
                }
                None => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    }
}
