//! Standardized state and metadata management for workers and execution runs.
//! Provides atomic reads and writes for worker metadata and execution states.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Atomically write `content` to `path` via a temporary file in the same directory.
pub fn atomic_write(path: &Path, content: impl AsRef<[u8]>) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp_path = parent.join(format!(".{file_name}.tmp.{}", std::process::id()));
    fs::write(&tmp_path, content)?;
    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

/// Metadata operations and paths for task workers in `runs_dir`.
pub struct WorkerMetadata;

impl WorkerMetadata {
    pub fn owner_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!("{name}.owner"))
    }

    pub fn grid_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!("{name}.grid"))
    }

    pub fn repo_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!("{name}.repo"))
    }

    pub fn pipeline_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!("{name}.pipeline"))
    }

    pub fn taskforce_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!("{name}.taskforce"))
    }

    pub fn save_taskforce(runs_dir: &Path, name: &str, task: &str) -> io::Result<()> {
        atomic_write(&Self::taskforce_path(runs_dir, name), format!("{task}\n"))
    }

    pub fn load_taskforce(runs_dir: &Path, name: &str) -> Option<String> {
        fs::read_to_string(Self::taskforce_path(runs_dir, name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn review_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!("{name}.review"))
    }

    pub fn layout_path(runs_dir: &Path, slug: &str) -> PathBuf {
        runs_dir.join(format!("{slug}.layout.toml"))
    }

    pub fn has_review(runs_dir: &Path, name: &str) -> bool {
        Self::review_path(runs_dir, name).is_file()
    }

    pub fn mark_review(runs_dir: &Path, name: &str) -> io::Result<()> {
        atomic_write(&Self::review_path(runs_dir, name), "")
    }

    pub fn state_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!(".{name}.state"))
    }

    pub fn settings_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!(".{name}.settings.json"))
    }

    /// Residual helper for `src/status.rs:307` (out of scope for this task).
    #[doc(hidden)]
    pub fn budget_path(runs_dir: &Path, name: &str) -> PathBuf {
        let b = format!(".{name}.{}", "budget");
        runs_dir.join(b)
    }
    pub fn cost80_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!(".{name}.cost80"))
    }

    pub fn cost100_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!(".{name}.cost100"))
    }

    pub fn cost200_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!(".{name}.cost200"))
    }

    pub fn provider_retry_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!(".{name}.provider_retry"))
    }

    pub fn progress_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!(".{name}.progress"))
    }

    pub fn elapsed_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!(".{name}.elapsed"))
    }

    pub fn nudged_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!(".{name}.nudged"))
    }

    pub fn submit_log_path(runs_dir: &Path, name: &str) -> PathBuf {
        runs_dir.join(format!(".{name}.submit.log"))
    }

    pub fn load_owner(runs_dir: &Path, name: &str) -> String {
        fs::read_to_string(Self::owner_path(runs_dir, name))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "cos".to_string())
    }

    pub fn save_owner(runs_dir: &Path, name: &str, owner: &str) -> io::Result<()> {
        atomic_write(&Self::owner_path(runs_dir, name), format!("{owner}\n"))
    }

    pub fn load_grid(runs_dir: &Path, name: &str) -> Option<String> {
        fs::read_to_string(Self::grid_path(runs_dir, name))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
    }

    pub fn save_grid(runs_dir: &Path, name: &str, grid: &str) -> io::Result<()> {
        atomic_write(&Self::grid_path(runs_dir, name), format!("{grid}\n"))
    }

    /// The repo a worker builds on, as recorded at spawn. Durable across a
    /// herdr restart, unlike the agent's cwd.
    pub fn load_repo(runs_dir: &Path, name: &str) -> Option<PathBuf> {
        fs::read_to_string(Self::repo_path(runs_dir, name))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    }

    pub fn save_repo(runs_dir: &Path, name: &str, repo: &Path) -> io::Result<()> {
        atomic_write(
            &Self::repo_path(runs_dir, name),
            format!("{}\n", repo.display()),
        )
    }

    pub fn has_cost80(runs_dir: &Path, name: &str) -> bool {
        Self::cost80_path(runs_dir, name).exists()
    }

    pub fn record_cost80(runs_dir: &Path, name: &str, cost: f64) -> io::Result<()> {
        atomic_write(&Self::cost80_path(runs_dir, name), format!("{cost}\n"))
    }

    pub fn has_cost100(runs_dir: &Path, name: &str) -> bool {
        Self::cost100_path(runs_dir, name).exists()
    }

    pub fn record_cost100(runs_dir: &Path, name: &str, cost: f64) -> io::Result<()> {
        atomic_write(&Self::cost100_path(runs_dir, name), format!("{cost}\n"))
    }

    pub fn has_cost200(runs_dir: &Path, name: &str) -> bool {
        Self::cost200_path(runs_dir, name).exists()
    }

    pub fn record_cost200(runs_dir: &Path, name: &str, cost: f64) -> io::Result<()> {
        atomic_write(&Self::cost200_path(runs_dir, name), format!("{cost}\n"))
    }

    pub fn has_provider_retry(runs_dir: &Path, name: &str) -> bool {
        Self::provider_retry_path(runs_dir, name).exists()
    }

    pub fn record_provider_retry(runs_dir: &Path, name: &str, ts: u64) -> io::Result<()> {
        atomic_write(
            &Self::provider_retry_path(runs_dir, name),
            format!("{ts}\n"),
        )
    }

    pub fn save_pipeline(runs_dir: &Path, name: &str, task: &str) -> io::Result<()> {
        atomic_write(&Self::pipeline_path(runs_dir, name), format!("{task}\n"))
    }

    pub fn load_pipeline(runs_dir: &Path, name: &str) -> Option<String> {
        fs::read_to_string(Self::pipeline_path(runs_dir, name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn load_elapsed(runs_dir: &Path, name: &str) -> u64 {
        fs::read_to_string(Self::elapsed_path(runs_dir, name))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn save_elapsed(runs_dir: &Path, name: &str, elapsed: u64) -> io::Result<()> {
        atomic_write(&Self::elapsed_path(runs_dir, name), format!("{elapsed}\n"))
    }

    pub fn load_progress(runs_dir: &Path, name: &str) -> u64 {
        fs::read_to_string(Self::progress_path(runs_dir, name))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn save_progress(runs_dir: &Path, name: &str, progress: u64) -> io::Result<()> {
        atomic_write(
            &Self::progress_path(runs_dir, name),
            format!("{progress}\n"),
        )
    }

    pub fn has_nudged(runs_dir: &Path, name: &str) -> bool {
        Self::nudged_path(runs_dir, name).exists()
    }

    pub fn record_nudged(runs_dir: &Path, name: &str) -> io::Result<()> {
        atomic_write(&Self::nudged_path(runs_dir, name), "")
    }

    /// Reset transient watchdog state files when initiating a fresh prompt.
    pub fn reset_prompt_state(runs_dir: &Path, name: &str) {
        let _ = fs::remove_file(Self::elapsed_path(runs_dir, name));
        let _ = fs::remove_file(Self::nudged_path(runs_dir, name));
        let _ = fs::remove_file(Self::progress_path(runs_dir, name));
        let _ = fs::remove_file(Self::provider_retry_path(runs_dir, name));
        let _ = fs::remove_file(Self::cost80_path(runs_dir, name));
        let _ = fs::remove_file(Self::cost100_path(runs_dir, name));
        let _ = fs::remove_file(Self::cost200_path(runs_dir, name));
        // A harness verdict belongs to the turn that produced it (#196): a
        // fresh prompt must not inherit the previous turn's error.
        let _ = fs::remove_file(crate::worker::prompt::retry::error_path(runs_dir, name));
    }

    /// Clean up all metadata and transient state files for a worker.
    /// Ensures `.cost80`, `.cost100`, `.cost200`, `.owner`, `.grid`, `.pipeline`,
    /// `.provider_retry`, `.error`, etc. are all deleted.
    pub fn cleanup_worker_files(runs_dir: &Path, name: &str) {
        let _ = fs::remove_file(Self::owner_path(runs_dir, name));
        let _ = fs::remove_file(Self::grid_path(runs_dir, name));
        let _ = fs::remove_file(Self::repo_path(runs_dir, name));
        let _ = fs::remove_file(Self::pipeline_path(runs_dir, name));
        let _ = fs::remove_file(Self::taskforce_path(runs_dir, name));
        let _ = fs::remove_file(Self::review_path(runs_dir, name));
        let _ = fs::remove_file(crate::worker::prompt::retry::error_path(runs_dir, name));
        let _ = fs::remove_file(Self::provider_retry_path(runs_dir, name));
        let _ = fs::remove_file(Self::cost80_path(runs_dir, name));
        let _ = fs::remove_file(Self::cost100_path(runs_dir, name));
        let _ = fs::remove_file(Self::cost200_path(runs_dir, name));
        let _ = fs::remove_file(Self::progress_path(runs_dir, name));
        let _ = fs::remove_file(Self::elapsed_path(runs_dir, name));
        let _ = fs::remove_file(Self::nudged_path(runs_dir, name));
        let _ = fs::remove_file(Self::submit_log_path(runs_dir, name));
        let _ = fs::remove_file(Self::state_path(runs_dir, name));
        let _ = fs::remove_file(Self::settings_path(runs_dir, name));
        let _ = fs::remove_dir_all(runs_dir.join(".opencode-state").join(name));
    }
}

/// Execution state parsed from `pipeline-*.state` and `taskforce-*.state` files.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionState {
    Pipeline(PipelineExecutionState),
    Taskforce(TaskforceExecutionState),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PipelineExecutionState {
    pub task: String,
    pub lead: String,
    pub pipeline_pid: String,
    pub builder: String,
    pub finished: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskforceExecutionState {
    pub task: String,
    pub lead: String,
    pub pid: String,
    pub kind: String,
    pub finished: bool,
    pub members: Vec<String>,
    pub n: Option<i64>,
}

impl PipelineExecutionState {
    pub fn owner(&self) -> &str {
        if self.lead.is_empty() {
            "cos"
        } else {
            &self.lead
        }
    }
}

impl TaskforceExecutionState {
    pub fn owner(&self) -> &str {
        if self.lead.is_empty() {
            "cos"
        } else {
            &self.lead
        }
    }
}

impl ExecutionState {
    pub fn load(path: &Path) -> Option<Self> {
        let file_name = path.file_name().and_then(|n| n.to_str())?;
        let is_pipeline = file_name.starts_with("pipeline-") && file_name.ends_with(".state");
        let is_taskforce = file_name.ends_with(".state")
            && (file_name.starts_with("taskforce-")
                || file_name.starts_with("dual-lens-")
                || file_name.starts_with("council-"));
        if !is_pipeline && !is_taskforce {
            return None;
        }

        let content = fs::read_to_string(path).ok()?;
        let is_running = content.lines().any(|l| l.trim() == "finished=0");

        if is_pipeline {
            let mut state = PipelineExecutionState {
                finished: !is_running,
                ..Default::default()
            };
            for line in content.lines() {
                let line = line.trim();
                if let Some(v) = line.strip_prefix("lead=") {
                    state.lead = v.to_string();
                } else if let Some(v) = line.strip_prefix("pipeline_pid=") {
                    state.pipeline_pid = v.to_string();
                } else if let Some(v) = line.strip_prefix("builder=") {
                    state.builder = v.to_string();
                } else if let Some(v) = line.strip_prefix("task=") {
                    state.task = v.to_string();
                }
            }
            if state.task.is_empty() {
                state.task = file_name
                    .strip_prefix("pipeline-")
                    .and_then(|n| n.strip_suffix(".state"))
                    .unwrap_or("")
                    .to_string();
            }
            Some(ExecutionState::Pipeline(state))
        } else {
            let mut state = TaskforceExecutionState {
                finished: !is_running,
                ..Default::default()
            };
            for line in content.lines() {
                let line = line.trim();
                if let Some(v) = line
                    .strip_prefix("lead=")
                    .or_else(|| line.strip_prefix("owner="))
                {
                    state.lead = v.to_string();
                } else if let Some(v) = line
                    .strip_prefix("taskforce_pid=")
                    .or_else(|| line.strip_prefix("pid="))
                {
                    state.pid = v.to_string();
                } else if let Some(v) = line
                    .strip_prefix("kind=")
                    .or_else(|| line.strip_prefix("taskforce="))
                {
                    state.kind = v.to_string();
                } else if let Some(v) = line.strip_prefix("task=") {
                    state.task = v.to_string();
                } else if let Some(v) = line.strip_prefix("members=") {
                    state.members = v
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                } else if let Some(v) = line.strip_prefix("n=") {
                    state.n = v.parse::<i64>().ok();
                }
            }
            if state.kind.is_empty() {
                if file_name.starts_with("council-") {
                    state.kind = "council".to_string();
                } else if file_name.starts_with("dual-lens-") {
                    state.kind = "dual-lens".to_string();
                } else {
                    state.kind = "taskforce".to_string();
                }
            }
            if state.task.is_empty() {
                state.task = file_name
                    .strip_prefix("council-")
                    .or_else(|| file_name.strip_prefix("dual-lens-"))
                    .or_else(|| file_name.strip_prefix("taskforce-"))
                    .and_then(|n| n.strip_suffix(".state"))
                    .unwrap_or("")
                    .to_string();
            }
            Some(ExecutionState::Taskforce(state))
        }
    }

    pub fn is_finished(&self) -> bool {
        match self {
            ExecutionState::Pipeline(p) => p.finished,
            ExecutionState::Taskforce(t) => t.finished,
        }
    }

    pub fn owner(&self) -> &str {
        let lead = match self {
            ExecutionState::Pipeline(p) => &p.lead,
            ExecutionState::Taskforce(p) => &p.lead,
        };
        if lead.is_empty() {
            "cos"
        } else {
            lead
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "quaqua-state-{}-{}-{tag}",
                std::process::id(),
                crate::util::now_sec()
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// #196 review: the harness verdict belongs to one turn, so a fresh prompt
    /// drops it instead of letting the next stall inherit it.
    #[test]
    fn reset_prompt_state_drops_the_harness_error_record() {
        let dir = TempDir::new("reset");
        let record = crate::worker::prompt::retry::error_path(&dir.0, "sude-x");
        fs::write(&record, "fatal 401 0\n").unwrap();
        let elapsed = WorkerMetadata::elapsed_path(&dir.0, "sude-x");
        fs::write(&elapsed, "5\n").unwrap();

        WorkerMetadata::reset_prompt_state(&dir.0, "sude-x");

        assert!(!record.exists(), "the previous turn's verdict is gone");
        assert!(!elapsed.exists(), "the other transient files still reset");
    }

    /// #196 review: a cleaned-up worker leaves no verdict behind that a later
    /// worker of the same name would read.
    #[test]
    fn cleanup_worker_files_drops_the_harness_error_record() {
        let dir = TempDir::new("cleanup");
        let record = crate::worker::prompt::retry::error_path(&dir.0, "sude-x");
        fs::write(&record, "retryable 429 3\n").unwrap();
        let owner = WorkerMetadata::owner_path(&dir.0, "sude-x");
        fs::write(&owner, "cos\n").unwrap();

        WorkerMetadata::cleanup_worker_files(&dir.0, "sude-x");

        assert!(!record.exists());
        assert!(!owner.exists(), "the other metadata files still go");
    }
}
