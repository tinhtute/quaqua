//! Status overview: workflow journal + orchestration state.
//!
//! Queries `<workspace_root>/runs/quaqua.db` for durable event-sourced workflows,
//! scans active workers from `herdr.agent_list()` and `runs/*.owner`,
//! and flags coordination anomalies like `WAITER-LOST` (issue #47).

use crate::worker::cleanup::WorkerTelemetryRow;
use crate::worker::prompt::cost::Tokens;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cap::is_task_worker_name;
use crate::config::{load_team_config, Config};
use crate::engine::journal_sqlite::{
    strongest_verdict_from_slice, SqliteJournal, VerdictLevel, VerdictRecord, VerdictSource,
};
use crate::git::{Git, GitOps};
use crate::herdr::HerdrOps;
use crate::state::WorkerMetadata;
#[path = "estimate.rs"]
pub mod estimate;

use estimate::{estimate, query_active_worker_start_ms, query_completed_durations, Estimate};

/// A workflow row extracted from the journal SQLite database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStatusRow {
    pub workflow_id: String,
    pub snap_seq: Option<u64>,
    pub last_event_kind: Option<String>,
    pub last_event_seq: Option<u64>,
    pub last_event_ts_ms: Option<u64>,
    pub phase: Option<String>,
}

/// Status of the worker report file (`runs/<name>.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStatus {
    None,
    Present,
    Unread,
}

/// Warning flags representing coordination defects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WarningFlag {
    WaiterLost,
    DoneNotCleaned,
    IdleUnprompted,
    Orphan,
}

impl WarningFlag {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WaiterLost => "WAITER-LOST",
            Self::DoneNotCleaned => "DONE-NOT-CLEANED",
            Self::IdleUnprompted => "IDLE-UNPROMPTED",
            Self::Orphan => "ORPHAN",
        }
    }
}

/// An orchestration row representing a Lead or task worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestrationRow {
    pub name: String,
    pub owner: String,
    pub team: String,
    pub agent_status: String,
    pub report_status: ReportStatus,
    pub phase: String,
    pub wait_time: String,
    pub flags: Vec<WarningFlag>,
    /// Why a blocked worker needs the Principal; `None` unless blocked (#179).
    pub blocked_reason: Option<BlockReason>,
    /// Remaining runtime estimate backed by historical completed runs (#180).
    pub estimate: Option<Estimate>,
}

/// The fixed set of reasons a blocked worker shows in `quaqua status` (#179).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    PlanToApprove,
    AsksPermission,
    SignedOut,
    QuestionForYou,
    /// No (or an unknown) cause recorded by the hook.
    NeedsInput,
}

impl BlockReason {
    /// Map the optional third token of `runs/.<name>.state` (`blocked <ts> <token>`).
    pub fn from_token(token: Option<&str>) -> Self {
        match token.map(str::trim) {
            Some("plan") => Self::PlanToApprove,
            Some("permission") => Self::AsksPermission,
            Some("signed-out") => Self::SignedOut,
            Some("question") => Self::QuestionForYou,
            _ => Self::NeedsInput,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PlanToApprove => "Plan to approve",
            Self::AsksPermission => "Asks permission",
            Self::SignedOut => "Signed out",
            Self::QuestionForYou => "Question for you",
            Self::NeedsInput => "Needs your input",
        }
    }
}

/// Reason for a blocked row: the hook's reason token when the state file says
/// `blocked`, else the generic fallback (e.g. herdr's screen said blocked).
fn blocked_reason(runs_dir: &Path, name: &str) -> BlockReason {
    let content = std::fs::read_to_string(crate::state::WorkerMetadata::state_path(runs_dir, name))
        .unwrap_or_default();
    let mut parts = content.split_whitespace();
    if parts.next() != Some("blocked") {
        return BlockReason::NeedsInput;
    }
    BlockReason::from_token(parts.nth(1))
}

/// A branch verdict row showing the strongest verdict and freshness flags (§ S2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchVerdictRow {
    pub branch: String,
    pub head_sha: String,
    pub verdict_sha: String,
    pub level: VerdictLevel,
    pub is_verified: bool,
    pub stale: bool,
    pub unresolved: bool,
    pub same_harness: bool,
}

/// Returns true if the verifier's harness matches the builder kind's harness in team.toml (§ S2).
pub fn is_same_harness(verdict: &VerdictRecord, cfg: Option<&Config>) -> bool {
    if verdict.source != VerdictSource::Verifier {
        return false;
    }

    let verifier_harness = if !verdict.harness.is_empty() {
        verdict.harness.clone()
    } else if let Some(vk) = &verdict.verifier_kind {
        cfg.and_then(|c| c.kinds.get(vk))
            .map(|k| k.harness.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let builder_harness = if let Some(bk) = &verdict.builder_kind {
        cfg.and_then(|c| c.kinds.get(bk))
            .map(|k| k.harness.clone())
            .unwrap_or_default()
    } else {
        let b_part = verdict
            .branch
            .strip_prefix("quaqua/")
            .unwrap_or(&verdict.branch);
        let inferred_kind = b_part.split('-').next().unwrap_or(b_part);
        cfg.and_then(|c| c.kinds.get(inferred_kind))
            .map(|k| k.harness.clone())
            .unwrap_or_default()
    };

    !verifier_harness.is_empty()
        && !builder_harness.is_empty()
        && verifier_harness == builder_harness
}

fn resolve_branch_head<G: GitOps>(git: &G, repo: &Path, branch: &str) -> Option<String> {
    let ref_name = if branch.starts_with("refs/") {
        branch.to_string()
    } else {
        format!("refs/heads/{branch}")
    };
    if let Ok(out) = git.run_in(repo, &["rev-parse", "--verify", "--quiet", &ref_name]) {
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !sha.is_empty() {
                return Some(sha);
            }
        }
    }
    if let Ok(out) = git.run_in(repo, &["rev-parse", "--verify", "--quiet", branch]) {
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !sha.is_empty() {
                return Some(sha);
            }
        }
    }
    None
}

fn resolve_candidate_repos(repo: &Path) -> Vec<PathBuf> {
    let mut repos = crate::merge::resolve_project_repos_from_index();
    if repo.join("projects/INDEX.md").is_file() {
        for r in crate::merge::resolve_project_repos_from_workspace(repo) {
            if !repos.contains(&r) {
                repos.push(r);
            }
        }
    }
    if repo.is_dir() && !repos.contains(&repo.to_path_buf()) {
        repos.push(repo.to_path_buf());
    }
    repos
}

fn resolve_branch_head_in_repos<G: GitOps>(
    git: &G,
    repos: &[PathBuf],
    branch: &str,
) -> Option<String> {
    for r in repos {
        if let Some(sha) = resolve_branch_head(git, r, branch) {
            return Some(sha);
        }
    }
    None
}

/// Query branch verdict rows from the journal and repository (§ S2).
pub fn query_branch_verdicts<G: GitOps>(
    db_path: &Path,
    repo: &Path,
    git: &G,
) -> Result<Vec<BranchVerdictRow>, String> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let journal = SqliteJournal::open(db_path)
        .map_err(|e| format!("db error: cannot open {}: {e}", db_path.display()))?;

    let all_verdicts = journal
        .load_all_verdicts()
        .map_err(|e| format!("db error: loading verdicts: {e}"))?;

    if all_verdicts.is_empty() {
        return Ok(Vec::new());
    }

    let team_cfg = load_team_config().ok();
    let candidate_repos = resolve_candidate_repos(repo);

    let mut branch_records: BTreeMap<String, Vec<&VerdictRecord>> = BTreeMap::new();
    for v in &all_verdicts {
        branch_records.entry(v.branch.clone()).or_default().push(v);
    }

    let mut rows = Vec::new();
    for (branch, records) in branch_records {
        let head_sha_opt = resolve_branch_head_in_repos(git, &candidate_repos, &branch);

        match head_sha_opt {
            Some(head_sha) => {
                let records_at_head: Vec<&VerdictRecord> = records
                    .iter()
                    .copied()
                    .filter(|v| v.sha == head_sha)
                    .collect();
                let verdict_at_head = strongest_verdict_from_slice(&records_at_head);

                if let Some(v) = verdict_at_head {
                    let same_harness = is_same_harness(&v, team_cfg.as_ref());
                    rows.push(BranchVerdictRow {
                        branch,
                        head_sha,
                        verdict_sha: v.sha.clone(),
                        level: v.level,
                        is_verified: v.is_verified(),
                        stale: false,
                        unresolved: false,
                        same_harness,
                    });
                } else {
                    let last_record = records.last().unwrap();
                    let records_at_last_sha: Vec<&VerdictRecord> = records
                        .iter()
                        .copied()
                        .filter(|v| v.sha == last_record.sha)
                        .collect();
                    let strongest_old = strongest_verdict_from_slice(&records_at_last_sha);

                    if let Some(v) = strongest_old {
                        let same_harness = is_same_harness(&v, team_cfg.as_ref());
                        let stale = head_sha != v.sha;
                        rows.push(BranchVerdictRow {
                            branch,
                            head_sha,
                            verdict_sha: v.sha.clone(),
                            level: v.level,
                            is_verified: false,
                            stale,
                            unresolved: false,
                            same_harness,
                        });
                    }
                }
            }
            None => {
                let last_record = records.last().unwrap();
                let records_at_last_sha: Vec<&VerdictRecord> = records
                    .iter()
                    .copied()
                    .filter(|v| v.sha == last_record.sha)
                    .collect();
                let strongest = strongest_verdict_from_slice(&records_at_last_sha);

                if let Some(v) = strongest {
                    let same_harness = is_same_harness(&v, team_cfg.as_ref());
                    rows.push(BranchVerdictRow {
                        branch,
                        head_sha: String::new(),
                        verdict_sha: v.sha.clone(),
                        level: v.level,
                        is_verified: false,
                        stale: false,
                        unresolved: true,
                        same_harness,
                    });
                }
            }
        }
    }

    Ok(rows)
}

/// Query workflow rows from `<workspace_root>/runs/quaqua.db`.
///
/// Missing or empty DB yields `Ok(Vec::new())` cleanly.
/// An existing but corrupted, unreadable, or schema-drifted DB returns `Err("db error: ...")`.
pub fn query_workflows(db_path: &Path) -> Result<Vec<WorkflowStatusRow>, String> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("db error: cannot open {}: {e}", db_path.display()))?;

    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(|e| {
            format!(
                "db error: failed to set busy timeout on {}: {e}",
                db_path.display()
            )
        })?;

    let has_events: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='events');",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("db error: checking events table: {e}"))?;

    let has_snapshots: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='snapshots');",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("db error: checking snapshots table: {e}"))?;

    if !has_events && !has_snapshots {
        return Ok(Vec::new());
    }

    let mut map: BTreeMap<String, WorkflowStatusRow> = BTreeMap::new();

    if has_events {
        let mut stmt = conn
            .prepare(
                "SELECT e.workflow_id, e.seq, e.ts_ms, e.kind \
                 FROM events e \
                 INNER JOIN ( \
                     SELECT workflow_id, MAX(seq) AS max_seq \
                     FROM events \
                     GROUP BY workflow_id \
                 ) m ON e.workflow_id = m.workflow_id AND e.seq = m.max_seq;",
            )
            .map_err(|e| format!("db error: preparing events query: {e}"))?;

        let rows = stmt
            .query_map([], |row| {
                let wf: String = row.get(0)?;
                let seq: i64 = row.get(1)?;
                let ts_ms: i64 = row.get(2)?;
                let kind: String = row.get(3)?;
                Ok((wf, seq as u64, ts_ms as u64, kind))
            })
            .map_err(|e| format!("db error: executing events query: {e}"))?;

        for r in rows {
            let (wf, seq, ts_ms, kind) =
                r.map_err(|e| format!("db error: decoding event row: {e}"))?;
            let entry = map.entry(wf.clone()).or_insert_with(|| WorkflowStatusRow {
                workflow_id: wf,
                snap_seq: None,
                last_event_kind: None,
                last_event_seq: None,
                last_event_ts_ms: None,
                phase: None,
            });
            entry.last_event_seq = Some(seq);
            entry.last_event_ts_ms = Some(ts_ms);
            entry.last_event_kind = Some(kind);
        }
    }

    if has_snapshots {
        let mut stmt = conn
            .prepare(
                "SELECT s.workflow_id, s.seq, s.state_json \
                 FROM snapshots s \
                 INNER JOIN ( \
                     SELECT workflow_id, MAX(seq) AS max_seq \
                     FROM snapshots \
                     GROUP BY workflow_id \
                 ) m ON s.workflow_id = m.workflow_id AND s.seq = m.max_seq;",
            )
            .map_err(|e| format!("db error: preparing snapshots query: {e}"))?;

        let rows = stmt
            .query_map([], |row| {
                let wf: String = row.get(0)?;
                let seq: i64 = row.get(1)?;
                let state_json: String = row.get(2)?;
                Ok((wf, seq as u64, state_json))
            })
            .map_err(|e| format!("db error: executing snapshots query: {e}"))?;

        for r in rows {
            let (wf, seq, state_json) =
                r.map_err(|e| format!("db error: decoding snapshot row: {e}"))?;
            let entry = map.entry(wf.clone()).or_insert_with(|| WorkflowStatusRow {
                workflow_id: wf,
                snap_seq: None,
                last_event_kind: None,
                last_event_seq: None,
                last_event_ts_ms: None,
                phase: None,
            });
            entry.snap_seq = Some(seq);
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&state_json) {
                if let Some(phase_str) = val.get("phase").and_then(|p| p.as_str()) {
                    entry.phase = Some(phase_str.to_string());
                }
            }
        }
    }
    for entry in map.values_mut() {
        if entry.last_event_kind.as_deref() == Some("workflow_cancelled") {
            entry.phase = Some("cancelled".to_string());
        }
    }

    Ok(map.into_values().collect())
}

/// Scan orchestration entities across `herdr.agent_list()` and `runs_dir`.
///
/// Propagates `herdr error: ...` on I/O or JSON decoding errors to avoid
/// emitting misleading `missing` statuses and false `ORPHAN` flags.
///
/// `cfg` decides which names are task workers; callers resolve it once so this
/// scan never reads process-global env.
fn lead_slug(name: &str) -> Option<&str> {
    name.strip_suffix("-lead").filter(|s| !s.is_empty())
}

/// Resolve the team for an entity by walking its owner chain starting from `name`.
/// - The first `<slug>-lead` found in the chain gives `<slug>` (a Lead row is its own team).
/// - Reaching `cos` with no lead in the chain yields `cos`.
/// - A missing or cyclic chain yields `?`.
pub fn resolve_team(runs_dir: &Path, name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "?".to_string();
    }

    let mut current = trimmed.to_string();
    let mut visited: Vec<String> = Vec::new();

    loop {
        if let Some(slug) = lead_slug(&current) {
            return slug.to_string();
        }
        if current == "cos" {
            return "cos".to_string();
        }
        if visited.contains(&current) {
            return "?".to_string();
        }
        visited.push(current.clone());

        let owner_path = runs_dir.join(format!("{current}.owner"));
        let content = match std::fs::read_to_string(&owner_path) {
            Ok(c) => c,
            Err(_) => return "?".to_string(),
        };

        let parent = content.trim();
        if parent.is_empty() {
            return "?".to_string();
        }

        current = parent.to_string();
    }
}

/// Status of one named entity as `quaqua status` shows it: the hook-reported
/// state in `runs/.<name>.state` when it is fresh, else herdr's screen status,
/// else `missing` when herdr does not list the agent at all.
fn resolved_status(
    runs_dir: &Path,
    name: &str,
    live_agents: &BTreeMap<String, String>,
    now_sec: u64,
) -> String {
    let screen = live_agents
        .get(name)
        .map(String::as_str)
        .unwrap_or("missing");
    crate::liveness::resolve_worker_state(
        runs_dir,
        name,
        screen,
        now_sec,
        crate::liveness::DEFAULT_STATE_STALENESS_SEC,
    )
    .as_str()
    .to_string()
}

pub fn scan_orchestration<H: HerdrOps>(
    runs_dir: &Path,
    herdr: &H,
    cfg: &Config,
) -> Result<Vec<OrchestrationRow>, String> {
    let json_str = herdr
        .agent_list()
        .map_err(|e| format!("herdr error: failed to query agent list: {e}"))?;

    let v: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("herdr error: invalid agent list JSON: {e}"))?;

    let arr = v
        .get("result")
        .and_then(|r| r.get("agents"))
        .and_then(|a| a.as_array())
        .ok_or_else(|| "herdr error: missing result.agents array in response".to_string())?;

    let mut live_agents: BTreeMap<String, String> = BTreeMap::new();
    for agent in arr {
        if let Some(name) = agent.get("name").and_then(|n| n.as_str()) {
            let st = agent
                .get("agent_status")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            live_agents.insert(name.to_string(), st.to_string());
        }
    }

    let mut entity_names: BTreeSet<String> = BTreeSet::new();
    for name in live_agents.keys() {
        entity_names.insert(name.clone());
    }

    if let Ok(entries) = std::fs::read_dir(runs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(worker) = fname.strip_suffix(".owner") {
                    if !worker.starts_with('.') {
                        entity_names.insert(worker.to_string());
                    }
                }
            }
        }
    }
    let db_path = runs_dir.join("quaqua.db");
    let duration_groups = query_completed_durations(&db_path).unwrap_or_default();
    let db_conn = if db_path.exists() {
        rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()
    } else {
        None
    };

    let now_sec = crate::util::now_sec();
    let mut rows = Vec::new();
    for name in entity_names {
        let owner = WorkerMetadata::load_owner(runs_dir, &name);
        // A fresh `runs/.<name>.state` (written by the agent's own Claude Code
        // hooks) wins over herdr's screen inference; with no fresh state file
        // this is exactly the screen status.
        let agent_status = resolved_status(runs_dir, &name, &live_agents, now_sec);

        let report_path = runs_dir.join(format!("{name}.md"));
        let report_exists = report_path.is_file();

        // Check quaqua-owned `.read` marker first.
        // The marker is only valid if its mtime >= report file mtime (not stale).
        // Fallback: check if report file access time is at or before modified time.
        let read_marker_path = runs_dir.join(format!(".{name}.read"));
        let is_unread = if report_exists {
            let marker_valid = if read_marker_path.is_file() {
                if let (Ok(m_meta), Ok(r_meta)) =
                    (read_marker_path.metadata(), report_path.metadata())
                {
                    if let (Ok(m_mtime), Ok(r_mtime)) = (m_meta.modified(), r_meta.modified()) {
                        m_mtime >= r_mtime
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };

            if marker_valid {
                false
            } else if let Ok(meta) = std::fs::metadata(&report_path) {
                if let (Ok(atime), Ok(mtime)) = (meta.accessed(), meta.modified()) {
                    atime <= mtime
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        let mut flags = Vec::new();

        let worker_running = agent_status == "working";
        let owner_is_cos = owner == "cos";

        let owner_inactive = if live_agents.contains_key(&owner) {
            matches!(
                resolved_status(runs_dir, &owner, &live_agents, now_sec).as_str(),
                "idle" | "done"
            )
        } else {
            !owner_is_cos
        };

        // WAITER-LOST:
        // 1. Orchestrator is inactive while its worker is still running.
        // 2. Report exists and nobody has read it (atime <= mtime).
        let waiter_lost_running = worker_running && owner_inactive;
        let waiter_lost_report = report_exists && is_unread;

        if waiter_lost_running || waiter_lost_report {
            flags.push(WarningFlag::WaiterLost);
        }

        let owner_file_exists = WorkerMetadata::owner_path(runs_dir, &name).exists();
        if owner_file_exists
            && (agent_status == "done"
                || agent_status == "dead"
                || (report_exists && agent_status != "working"))
        {
            flags.push(WarningFlag::DoneNotCleaned);
        }

        let has_session = db_conn
            .as_ref()
            .and_then(|conn| {
                let pattern = format!("worker:{name}#%");
                conn.query_row(
                    "SELECT 1 FROM events WHERE workflow_id LIKE ? LIMIT 1;",
                    [&pattern],
                    |_| Ok(()),
                )
                .ok()
            })
            .is_some();
        let has_progress = WorkerMetadata::progress_path(runs_dir, &name).exists();
        if is_task_worker_name(&name, cfg)
            && agent_status == "idle"
            && !has_session
            && !has_progress
        {
            flags.push(WarningFlag::IdleUnprompted);
        }

        if !owner_is_cos && !live_agents.contains_key(&owner) && owner_file_exists {
            flags.push(WarningFlag::Orphan);
        }

        let report_status = if !report_exists {
            ReportStatus::None
        } else if is_unread {
            ReportStatus::Unread
        } else {
            ReportStatus::Present
        };

        let phase = std::fs::read_to_string(WorkerMetadata::pipeline_path(runs_dir, &name))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".to_string());

        let mut elapsed = WorkerMetadata::load_elapsed(runs_dir, &name);
        if elapsed == 0 && agent_status == "working" {
            if let Some(conn) = &db_conn {
                if let Some(start_ms) = query_active_worker_start_ms(conn, &name) {
                    elapsed = now_sec.saturating_sub(start_ms / 1000);
                }
            }
        }

        let estimate = if agent_status == "working" {
            let kind = crate::config::kind_of_worker(cfg, &name);
            let playbook =
                crate::worker::prompt::playbook_check::PlaybookPin::read(runs_dir, &name)
                    .map(|p| p.name);
            match (kind, playbook) {
                (Some(k), Some(p)) => {
                    let key = (k.to_string(), p);
                    duration_groups.get(&key).and_then(|samples| {
                        estimate(samples, std::time::Duration::from_secs(elapsed))
                    })
                }
                _ => None,
            }
        } else {
            None
        };

        let elapsed_str = if elapsed > 0 {
            if elapsed >= 60 {
                format!("{}m", elapsed / 60)
            } else {
                format!("{elapsed}s")
            }
        } else {
            "-".to_string()
        };

        let wait_time = match &estimate {
            Some(est) => {
                let base = if elapsed > 0 {
                    elapsed_str
                } else {
                    "0s".to_string()
                };
                format!("{base} ({})", est.label())
            }
            None => elapsed_str,
        };

        flags.sort();
        flags.dedup();

        let reason = (agent_status == "blocked").then(|| blocked_reason(runs_dir, &name));
        rows.push(OrchestrationRow {
            name: name.clone(),
            owner,
            team: resolve_team(runs_dir, &name),
            agent_status,
            report_status,
            phase,
            wait_time,
            flags,
            blocked_reason: reason,
            estimate,
        });
    }

    Ok(rows)
}

/// Format workflow and orchestration status as a clean text table.
pub fn format_status_report(
    workflows: Result<&[WorkflowStatusRow], &str>,
    orchestration: Result<&[OrchestrationRow], &str>,
    verdicts: Result<&[BranchVerdictRow], &str>,
) -> String {
    format_status_report_opts(workflows, orchestration, verdicts, true)
}

/// A workflow whose last event ends it: nobody can act on the row.
/// `workflow_completed` with phase `disputed` stays (it needs a decision).
fn is_terminal_workflow(w: &WorkflowStatusRow) -> bool {
    match w.last_event_kind.as_deref() {
        Some("workflow_completed") => w.phase.as_deref() != Some("disputed"),
        Some("workflow_cancelled" | "verdict" | "playbook_gap") => true,
        _ => false,
    }
}

fn push_hidden_line(out: &mut String, hidden: usize) {
    if hidden > 0 {
        out.push_str(&format!("{hidden} hidden (quaqua status --all)\n"));
    }
}

/// Status report; `all == false` hides rows nobody can act on (see `StatusArgs`).
pub fn format_status_report_opts(
    workflows: Result<&[WorkflowStatusRow], &str>,
    orchestration: Result<&[OrchestrationRow], &str>,
    verdicts: Result<&[BranchVerdictRow], &str>,
    all: bool,
) -> String {
    let mut out = String::new();

    out.push_str("=== Workflows (runs/quaqua.db) ===\n");
    match workflows {
        Ok([]) => {
            out.push_str("no workflows\n");
        }
        Ok(all_wfs) => {
            let wfs: Vec<&WorkflowStatusRow> = all_wfs
                .iter()
                .filter(|w| all || !is_terminal_workflow(w))
                .collect();
            let hidden = all_wfs.len() - wfs.len();
            if wfs.is_empty() {
                out.push_str("no workflows\n");
            } else {
                out.push_str(&format!(
                    "{:<20} {:<10} {:<20} {:<10} {:<12} {:<10}\n",
                    "WORKFLOW_ID", "SNAP_SEQ", "LAST_EVENT", "EVENT_SEQ", "LAST_TS", "PHASE"
                ));
            }
            for wf in wfs {
                let snap = wf
                    .snap_seq
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let ev_kind = wf.last_event_kind.as_deref().unwrap_or("-");
                let ev_seq = wf
                    .last_event_seq
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let ev_ts = wf
                    .last_event_ts_ms
                    .map(|t| format!("{t}ms"))
                    .unwrap_or_else(|| "-".to_string());
                let phase = wf.phase.as_deref().unwrap_or("-");
                out.push_str(&format!(
                    "{:<20} {:<10} {:<20} {:<10} {:<12} {:<10}\n",
                    wf.workflow_id, snap, ev_kind, ev_seq, ev_ts, phase
                ));
            }
            push_hidden_line(&mut out, hidden);
        }
        Err(err) => {
            out.push_str(&format!("{err}\n"));
        }
    }

    out.push_str("\n=== Orchestration ===\n");
    match orchestration {
        Ok([]) => {
            out.push_str("no active agents or workers\n");
        }
        Ok(orch_rows) => {
            let wait_width = orch_rows
                .iter()
                .map(|r| r.wait_time.len())
                .max()
                .unwrap_or(4)
                .max(8);
            out.push_str(&format!(
                "{:<18} {:<16} {:<12} {:<10} {:<8} {:<10} {:<wait_width$} {}\n",
                "NAME", "OWNER", "TEAM", "STATUS", "REPORT", "PHASE", "WAIT", "WARNINGS"
            ));
            for r in orch_rows {
                let rep_str = match r.report_status {
                    ReportStatus::None => "none",
                    ReportStatus::Present => "present",
                    ReportStatus::Unread => "unread",
                };
                let flags_str = if r.flags.is_empty() {
                    "-".to_string()
                } else {
                    r.flags
                        .iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                out.push_str(&format!(
                    "{:<18} {:<16} {:<12} {:<10} {:<8} {:<10} {:<wait_width$} {}\n",
                    r.name,
                    r.owner,
                    r.team,
                    r.agent_status,
                    rep_str,
                    r.phase,
                    r.wait_time,
                    flags_str
                ));
                if let Some(reason) = r.blocked_reason {
                    out.pop();
                    out.push_str(&format!(" · {}\n", reason.label()));
                }
            }
        }
        Err(err) => {
            out.push_str(&format!("{err}\n"));
        }
    }

    out.push_str("\n=== Verdicts ===\n");
    match verdicts {
        Ok([]) => {
            out.push_str("no verdicts\n");
        }
        Ok(all_rows) => {
            let v_rows: Vec<&BranchVerdictRow> =
                all_rows.iter().filter(|r| all || !r.unresolved).collect();
            let hidden = all_rows.len() - v_rows.len();
            if v_rows.is_empty() {
                out.push_str("no verdicts\n");
            } else {
                out.push_str(&format!(
                    "{:<24} {:<12} {:<20} {}\n",
                    "BRANCH", "HEAD", "VERDICT", "FLAGS"
                ));
            }
            for r in v_rows {
                let mut flags = Vec::new();
                if r.unresolved {
                    flags.push("unresolved");
                }
                if r.stale {
                    flags.push("stale");
                }
                if r.same_harness {
                    flags.push("same-harness");
                }
                let flags_str = if flags.is_empty() {
                    "ok".to_string()
                } else {
                    flags.join(", ")
                };
                let head_str = if r.head_sha.len() >= 7 {
                    &r.head_sha[..7]
                } else if r.head_sha.is_empty() {
                    "-"
                } else {
                    &r.head_sha
                };
                out.push_str(&format!(
                    "{:<24} {:<12} {:<20} {}\n",
                    r.branch,
                    head_str,
                    r.level.as_str(),
                    flags_str
                ));
            }
            push_hidden_line(&mut out, hidden);
        }
        Err(err) => {
            out.push_str(&format!("{err}\n"));
        }
    }

    out
}

/// Render full status from explicit DB and runs directory paths.
/// Unfiltered (`--all`) report.
pub fn render_status<H: HerdrOps>(
    db_path: &Path,
    runs_dir: &Path,
    herdr: &H,
) -> Result<String, String> {
    let git = Git::new();
    render_status_with_git(db_path, runs_dir, herdr, &git)
}

/// Unfiltered (`--all`) report with explicit Git implementation.
pub fn render_status_with_git<H: HerdrOps, G: GitOps>(
    db_path: &Path,
    runs_dir: &Path,
    herdr: &H,
    git: &G,
) -> Result<String, String> {
    render_status_opts(db_path, runs_dir, herdr, git, true)
}

/// Render status; `all == false` hides rows nobody can act on.
pub fn render_status_opts<H: HerdrOps, G: GitOps>(
    db_path: &Path,
    runs_dir: &Path,
    herdr: &H,
    git: &G,
    all: bool,
) -> Result<String, String> {
    let wf_res = query_workflows(db_path);
    let orch_res = load_team_config()
        .map_err(|(_, msg)| msg.trim_end().to_string())
        .and_then(|cfg| scan_orchestration(runs_dir, herdr, &cfg));
    let repo = runs_dir.parent().unwrap_or_else(|| Path::new("."));
    let verdicts_res = query_branch_verdicts(db_path, repo, git);
    Ok(format_status_report_opts(
        wf_res.as_deref().map_err(|e| e.as_str()),
        orch_res.as_deref().map_err(|e| e.as_str()),
        verdicts_res.as_deref().map_err(|e| e.as_str()),
        all,
    ))
}

/// Parsed plain-status arguments (everything except `--metrics`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusArgs {
    pub all: bool,
    pub help: bool,
}

pub fn parse_status_args(args: &[String]) -> Result<StatusArgs, String> {
    let mut parsed = StatusArgs::default();
    for a in args {
        match a.as_str() {
            "--all" => parsed.all = true,
            "--help" | "-h" => parsed.help = true,
            other => return Err(format!("unknown argument to status: {other}")),
        }
    }
    Ok(parsed)
}

pub fn status_help() -> String {
    "Usage: quaqua status [--all | --metrics[=<since>] [--since <dur>] [--owner <name>] [--json]]\n\
\n\
By default only rows someone can act on are shown. A section that hid rows ends\n\
with `N hidden (quaqua status --all)`.\n\
\n\
  --all    show every Workflows and Verdicts row\n\
\n\
Workflows hides rows whose last event is terminal: workflow_completed (except\n\
phase disputed), workflow_cancelled, verdict, playbook_gap.\n\
\n\
Verdicts FLAGS (`ok` = none set):\n\
  stale         the branch head moved after the verdict\n\
  unresolved    the branch resolves in no repo (hidden by default)\n\
  same-harness  verifier and builder run the same harness\n"
        .to_string()
}

/// CLI entrypoint for `quaqua status`.
pub fn handle_status<H: HerdrOps>(args: &[String], herdr: &H) -> Result<i32, String> {
    if args
        .iter()
        .any(|a| a == "--metrics" || a.starts_with("--metrics="))
    {
        let metrics_args = parse_metrics_args(args)?;
        let runs_dir = if let Ok(dir) = std::env::var("QUAQUA_RUNS_DIR") {
            PathBuf::from(dir)
        } else {
            crate::workspace::workspace_root().join("runs")
        };
        let out = render_metrics(&runs_dir, &metrics_args)?;
        print!("{out}");
        return Ok(0);
    }

    let status_args = match parse_status_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n\n{}", status_help());
            return Ok(2);
        }
    };
    if status_args.help {
        print!("{}", status_help());
        return Ok(0);
    }

    let runs_dir = if let Ok(dir) = std::env::var("QUAQUA_RUNS_DIR") {
        PathBuf::from(dir)
    } else {
        crate::workspace::workspace_root().join("runs")
    };
    let db_path = runs_dir.join("quaqua.db");
    let git = Git::new();
    let report = render_status_opts(&db_path, &runs_dir, herdr, &git, status_args.all)?;
    print!("{report}");
    Ok(0)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MetricsArgs {
    pub metrics: bool,
    pub since: String,
    pub owner: Option<String>,
    pub json: bool,
}

pub fn parse_metrics_args(args: &[String]) -> Result<MetricsArgs, String> {
    let mut parsed = MetricsArgs {
        metrics: false,
        since: "24h".to_string(),
        owner: None,
        json: false,
    };

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--metrics" {
            parsed.metrics = true;
        } else if let Some(val) = arg.strip_prefix("--metrics=") {
            parsed.metrics = true;
            if !val.is_empty() {
                parsed.since = val.to_string();
            }
        } else if arg == "--json" {
            parsed.json = true;
        } else if arg == "--since" {
            i += 1;
            if i < args.len() {
                parsed.since = args[i].clone();
            } else {
                return Err("missing value for --since".to_string());
            }
        } else if let Some(val) = arg.strip_prefix("--since=") {
            parsed.since = val.to_string();
        } else if arg == "--owner" {
            i += 1;
            if i < args.len() {
                parsed.owner = Some(args[i].clone());
            } else {
                return Err("missing value for --owner".to_string());
            }
        } else if let Some(val) = arg.strip_prefix("--owner=") {
            parsed.owner = Some(val.to_string());
        }
        i += 1;
    }
    let now_sec = crate::util::now_sec();
    let now_ms = now_sec * 1000;
    parse_since_cutoff(&parsed.since, now_ms)?;

    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PipelineTelemetryItem {
    pub ts: String,
    #[serde(default = "default_pipeline_rec")]
    pub rec: String,
    pub task: String,
    pub repo: String,
    pub builder: String,
    pub reviewer: String,
    pub review: String,
    pub rounds: u32,
    #[serde(default)]
    pub review_changed_outcome: bool,
    pub status: String,
    pub minutes: u64,
}

fn default_pipeline_rec() -> String {
    "pipeline".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricsReport {
    pub since: String,
    pub workers: Vec<WorkerTelemetryRow>,
    pub pipelines: Vec<PipelineTelemetryItem>,
    pub total: MetricsTotal,
    #[serde(default)]
    pub journal: JournalMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_error: Option<String>,
}

/// `playbook_gap` rollup for one playbook name (§ S5, counted in § S7).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaybookGapTally {
    pub events: usize,
    pub missing: usize,
    pub unreasoned: usize,
}

/// Journal-derived counts for the pstack success metrics (§ S7).
///
/// Verdicts, `playbook_gap` and watchdog exits come from `runs/quaqua.db` read
/// only. G1 (merges with no verdict at the incoming head) cannot come from the
/// journal alone: the engine records no merge event, so merge commits are listed
/// from git on the upstream `main` (or local `main`) and every integration
/// branch, windowed by merge commit date, and matched against the whole journal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalMetrics {
    /// Verdict events inside the `--since` window, one entry per level.
    pub verdicts: BTreeMap<String, usize>,
    pub verdicts_total: usize,
    /// `playbook_gap` events inside the window, per playbook name.
    pub playbook_gaps: BTreeMap<String, PlaybookGapTally>,
    pub playbook_gap_events: usize,
    pub playbook_gap_missing: usize,
    pub playbook_gap_unreasoned: usize,
    /// G1: merge commits scanned inside the window (the S0 baseline's unit).
    #[serde(default)]
    pub merges: usize,
    /// G1: incoming merge heads inside the window, checked against the whole journal.
    pub merges_checked: usize,
    /// Heads whose strongest verdict is not a verifier pass (G1 numerator).
    pub merges_without_verdict: usize,
    /// Split of `merges_without_verdict`: no journal record at all, versus a
    /// record whose strongest verdict is failed, blocked or inconclusive.
    #[serde(default)]
    pub merges_no_verdict: usize,
    #[serde(default)]
    pub merges_not_pass: usize,
    /// Up to five `target@sha` of the merges counted in `merges_without_verdict`.
    #[serde(default)]
    pub merges_missing_samples: Vec<String>,
    pub merge_repos: usize,
    /// G1 per repository and ref, so a number can be traced to its source.
    #[serde(default)]
    pub merges_per_repo: Vec<MergeRepoStat>,
    /// G1: whether any contributing ref scan hit the 500-merge cap.
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        alias = "merges_truncated"
    )]
    pub truncated: bool,
    /// G2: watchdog exits inside the window.
    pub accepted_reports: usize,
    pub exit6: usize,
    pub exit6_resumed: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MetricsTotal {
    pub workers: usize,
    pub usd: f64,
    #[serde(default)]
    pub unpriced: usize,
    #[serde(default)]
    pub claude_unpriced: usize,
    pub wall_sec: u64,
}

/// G1 counts for one repository: the refs it was read from and what they held.
///
/// A merge reachable from several refs of the same repo is counted once, under
/// the first ref: sums here match the totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeRepoStat {
    pub repo: String,
    /// Refs that contributed at least one merge, in scan order.
    #[serde(default)]
    pub refs: Vec<MergeRefStat>,
    /// Merge commits counted for this repo (the S0 baseline's unit).
    #[serde(default)]
    pub merges: usize,
    /// Incoming heads checked (one per ordinary merge).
    pub checked: usize,
    pub without_verdict: usize,
}

/// One ref a repo's G1 count was read from, with the merges it contributed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeRefStat {
    pub target: String,
    pub merges: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

pub fn parse_since_cutoff(since: &str, now_ms: u64) -> Result<u64, String> {
    let trimmed = since.trim();
    if let Some(num_str) = trimmed.strip_suffix('h') {
        if let Ok(hours) = num_str.parse::<u64>() {
            let delta = hours.saturating_mul(3600).saturating_mul(1000);
            return Ok(now_ms.saturating_sub(delta));
        }
    } else if let Some(num_str) = trimmed.strip_suffix('d') {
        if let Ok(days) = num_str.parse::<u64>() {
            let delta = days.saturating_mul(86400).saturating_mul(1000);
            return Ok(now_ms.saturating_sub(delta));
        }
    } else if let Some(num_str) = trimmed.strip_suffix('m') {
        if let Ok(mins) = num_str.parse::<u64>() {
            let delta = mins.saturating_mul(60).saturating_mul(1000);
            return Ok(now_ms.saturating_sub(delta));
        }
    } else if let Some(ms) = crate::worker::prompt::cost::parse_iso_timestamp_ms(trimmed) {
        return Ok(ms);
    }
    Err(format!(
        "invalid --since value '{since}', expected <N>h, <N>d, <N>m, or ISO timestamp"
    ))
}

pub fn read_metrics(runs_dir: &Path, args: &MetricsArgs) -> MetricsReport {
    let now_sec = crate::util::now_sec();
    let now_ms = now_sec * 1000;
    let cutoff_ms = parse_since_cutoff(&args.since, now_ms).unwrap_or(0);

    let tele_path = runs_dir.join("telemetry.jsonl");
    let mut workers = Vec::new();
    let mut pipelines = Vec::new();

    if let Ok(file) = std::fs::File::open(&tele_path) {
        use std::io::BufRead;
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                continue;
            };

            let ts_ms = v
                .get("ts")
                .and_then(|t| t.as_str())
                .and_then(crate::worker::prompt::cost::parse_iso_timestamp_ms)
                .unwrap_or(0);

            if ts_ms < cutoff_ms {
                continue;
            }

            let rec = v.get("rec").and_then(|r| r.as_str());
            if rec == Some("worker") {
                if let Ok(row) = serde_json::from_str::<WorkerTelemetryRow>(trimmed) {
                    if let Some(o) = &args.owner {
                        if row.owner.as_deref() != Some(o.as_str()) {
                            continue;
                        }
                    }
                    workers.push(row);
                }
            } else {
                if let Ok(row) = serde_json::from_str::<PipelineTelemetryItem>(trimmed) {
                    pipelines.push(row);
                }
            }
        }
    }

    let mut total_usd = 0.0;
    let mut unpriced = 0usize;
    let mut claude_unpriced = 0usize;
    let mut total_wall_sec = 0u64;

    for w in &workers {
        if let Some(usd) = w.usd {
            total_usd += usd;
        } else {
            unpriced += 1;
            if w.harness
                .as_deref()
                .and_then(crate::harness::Harness::parse)
                == Some(crate::harness::Harness::Claude)
                || w.spend_src == "claude-tokens"
            {
                claude_unpriced += 1;
            }
        }
        if let Some(wall) = w.wall_sec {
            total_wall_sec += wall;
        }
    }
    let worker_count = workers.len();
    let repos = metric_repos(runs_dir);
    let (journal, journal_error) =
        match load_journal_metrics(&runs_dir.join("quaqua.db"), cutoff_ms, &repos, &Git::new()) {
            Ok(metrics) => (metrics, None),
            Err(err) => (JournalMetrics::default(), Some(err)),
        };
    MetricsReport {
        since: args.since.clone(),
        workers,
        pipelines,
        total: MetricsTotal {
            workers: worker_count,
            usd: (total_usd * 10000.0).round() / 10000.0,
            unpriced,
            claude_unpriced,
            wall_sec: total_wall_sec,
        },
        journal,
        journal_error,
    }
}

/// Upper bound on merge commits scanned per integration target.
const MERGE_SCAN_LIMIT: usize = 500;

/// One merge commit and the heads it brought in (every parent after the first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergeCommit {
    pub repo: PathBuf,
    pub target: String,
    pub sha: String,
    pub incoming: Vec<String>,
    pub target_truncated: bool,
}

/// One integration target scanned for merge commits and whether its scan was capped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergeRefScan {
    pub repo: PathBuf,
    pub target: String,
    pub truncated: bool,
}

/// The result of scanning integration targets for merge commits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MergeScan {
    pub commits: Vec<MergeCommit>,
    pub scans: Vec<MergeRefScan>,
}

impl MergeScan {
    pub fn is_truncated(&self) -> bool {
        self.scans.iter().any(|s| s.truncated)
    }

    #[cfg(test)]
    pub fn from_commits(commits: Vec<MergeCommit>) -> Self {
        Self {
            commits,
            scans: Vec::new(),
        }
    }
}

impl std::ops::Deref for MergeScan {
    type Target = [MergeCommit];

    fn deref(&self) -> &Self::Target {
        &self.commits
    }
}

/// Repos whose integration branches G1 looks at: every `projects/INDEX.md` row
/// plus the workspace root itself when it is a checkout.
pub(crate) fn metric_repos(runs_dir: &Path) -> Vec<PathBuf> {
    let ws_root = runs_dir.parent().unwrap_or_else(|| Path::new("."));
    let mut repos = crate::merge::resolve_project_repos_from_workspace(ws_root);
    if ws_root.join(".git").exists() {
        let root = ws_root.to_path_buf();
        if !repos.contains(&root) {
            repos.push(root);
        }
    }
    repos
}

/// True when `branch` is an integration branch G1 counts merges into.
///
/// The plan names `<slug>-integ`; this programme's own branches are
/// `quaqua/pstack-int` and `quaqua/pstack-int2`, so the suffix test covers
/// `-integ` and `-int[<digits>]`.
pub(crate) fn is_integration_branch(branch: &str) -> bool {
    let short = branch.rsplit('/').next().unwrap_or(branch);
    if short.ends_with("-integ") {
        return true;
    }
    short
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .ends_with("-int")
}

/// Ref G1 reads `main` from: the upstream tracking ref when it exists, else the
/// local branch. `origin/main` is what the S0 baseline measured, and it sees PR
/// merges that a local `main` does not.
fn main_target<G: GitOps>(repo: &Path, git: &G, local_heads: &[String]) -> Option<String> {
    if git.rev_parse_commit(repo, "refs/remotes/origin/main") {
        return Some("origin/main".to_string());
    }
    local_heads.iter().find(|b| b.as_str() == "main").cloned()
}

fn merge_targets<G: GitOps>(repo: &Path, git: &G) -> Vec<String> {
    let Ok(out) = git.run_in(
        repo,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    ) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let heads: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(str::to_string)
        .collect();
    let mut targets: Vec<String> = Vec::new();
    if let Some(main) = main_target(repo, git, &heads) {
        targets.push(main);
    }
    targets.extend(heads.iter().filter(|b| is_integration_branch(b)).cloned());
    targets.sort();
    targets.dedup();
    targets
}

/// Lists merge commits reachable from each G1 target, newest first, windowed by
/// committer date and capped at [`MERGE_SCAN_LIMIT`] per target. Deduplicated by
/// SHA across targets, so a merge reachable from `main` and from an integration
/// branch is counted once.
fn collect_merge_commits<G: GitOps>(repos: &[PathBuf], git: &G, cutoff_ms: u64) -> MergeScan {
    let fetch_limit = (MERGE_SCAN_LIMIT + 1).to_string();
    let since = format!("--since=@{}", cutoff_ms / 1000);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut commits = Vec::new();
    let mut scans = Vec::new();
    for repo in repos {
        for target in merge_targets(repo, git) {
            let mut args = vec![
                "log",
                "--merges",
                "-n",
                fetch_limit.as_str(),
                "--format=%H %P",
            ];
            if cutoff_ms > 0 {
                args.push(since.as_str());
            }
            args.push(target.as_str());
            let Ok(out) = git.run_in(repo, &args) else {
                continue;
            };
            if !out.status.success() {
                continue;
            }
            let mut target_commits = Vec::new();
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let mut it = line.split_whitespace();
                let Some(sha) = it.next() else { continue };
                let parents: Vec<&str> = it.collect();
                if parents.len() < 2 {
                    continue;
                }
                target_commits.push((
                    sha.to_string(),
                    parents[1..]
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>(),
                ));
            }
            let is_truncated = target_commits.len() > MERGE_SCAN_LIMIT;
            scans.push(MergeRefScan {
                repo: repo.clone(),
                target: target.clone(),
                truncated: is_truncated,
            });
            target_commits.truncate(MERGE_SCAN_LIMIT);
            for (sha, incoming) in target_commits {
                if !seen.insert(sha.clone()) {
                    continue;
                }
                commits.push(MergeCommit {
                    repo: repo.clone(),
                    target: target.clone(),
                    sha,
                    incoming,
                    target_truncated: is_truncated,
                });
            }
        }
    }
    MergeScan { commits, scans }
}

fn sha_matches(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a))
}

/// A repo path as printed: `~/...` under `$HOME`, absolute otherwise.
fn display_repo(repo: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Ok(rest) = repo.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    repo.display().to_string()
}

/// G1 counts over the scanned merge commits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MergeTally {
    pub merges: usize,
    pub checked: usize,
    pub without_verdict: usize,
    pub no_verdict: usize,
    pub not_pass: usize,
    pub samples: Vec<String>,
    pub repos: Vec<MergeRepoStat>,
    pub truncated: bool,
}

/// G1: counts incoming merge heads and how many lack a verifier pass.
///
/// Coverage is [`strongest_verdict_from_slice`] (the single S1 selection rule)
/// plus [`VerdictRecord::is_verified`]: the strongest verdict for the head must
/// be an independent verifier pass. A head with no record, only a self-report,
/// or a failed/blocked/inconclusive verifier verdict is not covered.
pub(crate) fn count_merges_without_pass(
    scan: &MergeScan,
    verdicts: &[VerdictRecord],
) -> MergeTally {
    let mut tally = MergeTally {
        truncated: scan.is_truncated(),
        ..MergeTally::default()
    };
    let mut per_repo: BTreeMap<String, MergeRepoStat> = BTreeMap::new();
    for commit in &scan.commits {
        let repo = display_repo(&commit.repo);
        tally.merges += 1;
        let entry = per_repo
            .entry(repo.clone())
            .or_insert_with(|| MergeRepoStat {
                repo,
                ..MergeRepoStat::default()
            });
        match entry.refs.iter_mut().find(|r| r.target == commit.target) {
            Some(r) => {
                r.merges += 1;
                r.truncated |= commit.target_truncated;
            }
            None => entry.refs.push(MergeRefStat {
                target: commit.target.clone(),
                merges: 1,
                truncated: commit.target_truncated,
            }),
        }
        entry.merges += 1;
        for head in &commit.incoming {
            tally.checked += 1;
            entry.checked += 1;
            let matching: Vec<&VerdictRecord> = verdicts
                .iter()
                .filter(|v| sha_matches(&v.sha, head))
                .collect();
            let strongest = strongest_verdict_from_slice(&matching);
            if !strongest.as_ref().is_some_and(|v| v.is_verified()) {
                tally.without_verdict += 1;
                entry.without_verdict += 1;
                if strongest.is_some() {
                    tally.not_pass += 1;
                } else {
                    tally.no_verdict += 1;
                }
                if tally.samples.len() < 5 {
                    let short: String = head.chars().take(7).collect();
                    tally.samples.push(format!("{}@{short}", commit.target));
                }
            }
        }
    }
    for ref_scan in &scan.scans {
        if ref_scan.truncated {
            tally.truncated = true;
            let repo = display_repo(&ref_scan.repo);
            let entry = per_repo
                .entry(repo.clone())
                .or_insert_with(|| MergeRepoStat {
                    repo,
                    ..MergeRepoStat::default()
                });
            match entry.refs.iter_mut().find(|r| r.target == ref_scan.target) {
                Some(r) => {
                    r.truncated = true;
                }
                None => {
                    entry.refs.push(MergeRefStat {
                        target: ref_scan.target.clone(),
                        merges: 0,
                        truncated: true,
                    });
                }
            }
        }
    }
    if !tally.truncated {
        tally.truncated = scan.commits.iter().any(|c| c.target_truncated);
    }
    for (repo_name, entry) in &mut per_repo {
        entry.refs.sort_by_key(|r| {
            scan.scans
                .iter()
                .position(|s| display_repo(&s.repo) == *repo_name && s.target == r.target)
                .unwrap_or(usize::MAX)
        });
    }
    tally.repos = per_repo.into_values().collect();
    tally
}

/// Fills the G1 fields of `metrics`: merges come from git, coverage from the
/// whole journal (a verdict older than the window still covers its merge).
fn fill_merge_metrics<G: GitOps>(
    metrics: &mut JournalMetrics,
    repos: &[PathBuf],
    git: &G,
    cutoff_ms: u64,
    verdicts: &[VerdictRecord],
) {
    let scan = collect_merge_commits(repos, git, cutoff_ms);
    let tally = count_merges_without_pass(&scan, verdicts);
    metrics.merges = tally.merges;
    metrics.merges_checked = tally.checked;
    metrics.merges_without_verdict = tally.without_verdict;
    metrics.merges_no_verdict = tally.no_verdict;
    metrics.merges_not_pass = tally.not_pass;
    metrics.merges_missing_samples = tally.samples;
    metrics.merge_repos = tally
        .repos
        .iter()
        .map(|s| s.repo.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    metrics.merges_per_repo = tally.repos;
    metrics.truncated = tally.truncated;
}

/// Splits `worker:<name>#<n>` into its base and session number.
fn session_base(id: &str) -> Option<(&str, u64)> {
    let (base, n) = id.rsplit_once('#')?;
    Some((base, n.parse().ok()?))
}

/// G2 numerator: an exit-6 session counts as resumed when a later session
/// (`worker:<name>#<n+>`) exists for the same worker.
fn exit6_was_resumed(wf_id: &str, max_session: &BTreeMap<String, u64>) -> bool {
    let Some((base, n)) = session_base(wf_id) else {
        return false;
    };
    max_session.get(base).is_some_and(|max| *max > n)
}

fn load_journal_metrics<G: GitOps>(
    db_path: &Path,
    cutoff_ms: u64,
    repos: &[PathBuf],
    git: &G,
) -> Result<JournalMetrics, String> {
    if !db_path.is_file() {
        return Err(format!("journal not found at {}", db_path.display()));
    }
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("db error: cannot open {}: {e}", db_path.display()))?;
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(|e| format!("db error: cannot set busy timeout: {e}"))?;

    let has_events: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='events');",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("db error: checking events table: {e}"))?;
    if !has_events {
        // G1 comes from git alone: a journal without verdicts must still report
        // the merges it cannot cover, not `0 checked`.
        let mut metrics = JournalMetrics::default();
        fill_merge_metrics(&mut metrics, repos, git, cutoff_ms, &[]);
        return Ok(metrics);
    }

    let mut metrics = JournalMetrics::default();

    // Verdicts: level counts inside the window, and every record for the G1
    // check (an old verdict still covers a merge inside the window).
    let mut all_verdicts: Vec<VerdictRecord> = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT ts_ms, payload_json FROM events WHERE kind = 'verdict' ORDER BY ts_ms ASC;",
            )
            .map_err(|e| format!("db error: preparing verdict query: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("db error: reading verdicts: {e}"))?;
        for row in rows {
            let (ts_ms, payload) =
                row.map_err(|e| format!("db error: reading verdict row: {e}"))?;
            let Ok(record) = serde_json::from_str::<VerdictRecord>(&payload) else {
                continue;
            };
            if (ts_ms.max(0) as u64) >= cutoff_ms {
                metrics.verdicts_total += 1;
                *metrics
                    .verdicts
                    .entry(record.level.as_str().to_string())
                    .or_insert(0) += 1;
            }
            all_verdicts.push(record);
        }
    }

    // playbook_gap events, rolled up by playbook name.
    {
        let mut stmt = conn
            .prepare("SELECT ts_ms, payload_json FROM events WHERE kind = 'playbook_gap' ORDER BY ts_ms ASC;")
            .map_err(|e| format!("db error: preparing playbook_gap query: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("db error: reading playbook gaps: {e}"))?;
        for row in rows {
            let (ts_ms, payload) =
                row.map_err(|e| format!("db error: reading playbook_gap row: {e}"))?;
            if (ts_ms.max(0) as u64) < cutoff_ms {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                continue;
            };
            let name = value
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("unknown")
                .to_string();
            let missing = value
                .get("missing")
                .and_then(|m| m.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            let unreasoned = value
                .get("unreasoned")
                .and_then(|m| m.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            let tally = metrics.playbook_gaps.entry(name).or_default();
            tally.events += 1;
            tally.missing += missing;
            tally.unreasoned += unreasoned;
            metrics.playbook_gap_events += 1;
            metrics.playbook_gap_missing += missing;
            metrics.playbook_gap_unreasoned += unreasoned;
        }
    }

    // G2: accepted reports (exit 0) and exit-6 sessions, with resumes.
    let max_session = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT workflow_id FROM events;")
            .map_err(|e| format!("db error: preparing session query: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("db error: reading sessions: {e}"))?;
        let mut max_session: BTreeMap<String, u64> = BTreeMap::new();
        for row in rows {
            let wf_id = row.map_err(|e| format!("db error: reading session id: {e}"))?;
            if let Some((base, n)) = session_base(&wf_id) {
                let entry = max_session.entry(base.to_string()).or_insert(0);
                *entry = (*entry).max(n);
            }
        }
        max_session
    };
    {
        let mut stmt = conn
            .prepare("SELECT workflow_id, ts_ms, payload_json FROM events WHERE kind = 'workflow_completed' ORDER BY ts_ms ASC;")
            .map_err(|e| format!("db error: preparing workflow_completed query: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("db error: reading workflow_completed: {e}"))?;
        for row in rows {
            let (wf_id, ts_ms, payload) =
                row.map_err(|e| format!("db error: reading workflow_completed row: {e}"))?;
            if (ts_ms.max(0) as u64) < cutoff_ms {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                continue;
            };
            let Some(exit) = value.get("exit").and_then(|e| e.as_i64()) else {
                continue;
            };
            match exit {
                0 => metrics.accepted_reports += 1,
                6 => {
                    metrics.exit6 += 1;
                    if exit6_was_resumed(&wf_id, &max_session) {
                        metrics.exit6_resumed += 1;
                    }
                }
                _ => {}
            }
        }
    }

    // G1: merges are not journaled, so list them from git.
    fill_merge_metrics(&mut metrics, repos, git, cutoff_ms, &all_verdicts);

    Ok(metrics)
}

fn format_token_metric(tok: Option<&Tokens>) -> String {
    let Some(t) = tok else {
        return "-".to_string();
    };
    let in_str = format_compact_number(t.in_tokens);
    let out_str = format_compact_number(t.out_tokens);
    let cache_str = format_compact_number(t.cache_read);
    format!("{in_str}/{out_str}/{cache_str}")
}

fn format_compact_number(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        format!("{:.1}M", m)
    } else if n >= 1_000 {
        let k = (n as f64 / 1000.0).round() as u64;
        format!("{k}k")
    } else {
        format!("{n}")
    }
}

fn format_duration_metric(sec: u64) -> String {
    if sec >= 3600 {
        let h = sec / 3600;
        let m = ((sec % 3600) as f64 / 60.0).round() as u64;
        if m > 0 {
            format!("{h}h{m}m")
        } else {
            format!("{h}h")
        }
    } else if sec >= 60 {
        let m = (sec as f64 / 60.0).round() as u64;
        format!("{m}m")
    } else {
        format!("{sec}s")
    }
}

pub fn render_metrics_text(report: &MetricsReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "=== Metrics (runs/telemetry.jsonl, since {}) ===\n",
        report.since
    ));
    let name_width = report
        .workers
        .iter()
        .map(|w| w.name.len())
        .max()
        .unwrap_or(4)
        .max(17);

    out.push_str(&format!(
        "{:<name_width$} kind/harness       owner  exit  wall/budget  $/budget      tokens in/out/cache   pipeline\n",
        "name"
    ));

    for w in &report.workers {
        let name = &w.name;
        let kind_harness = format!(
            "{}/{}",
            w.kind.as_deref().unwrap_or("-"),
            w.harness.as_deref().unwrap_or("-")
        );
        let owner = w.owner.as_deref().unwrap_or("-");
        let exit = w
            .exit
            .map(|e| e.to_string())
            .unwrap_or_else(|| "-".to_string());

        let wall_str = w
            .wall_sec
            .map(format_duration_metric)
            .unwrap_or_else(|| "-".to_string());
        let budget_str = w
            .budget_sec
            .map(format_duration_metric)
            .unwrap_or_else(|| "-".to_string());
        let wall_budget = format!("{wall_str}/{budget_str}");

        let cost_str = match w.usd {
            Some(c) if c < 0.1 => format!("{c:.3}"),
            Some(c) => format!("{c:.2}"),
            None => "-".to_string(),
        };
        let cost_budget_str = match w.usd_budget {
            Some(b) => format!("{b:.2}"),
            None => "-".to_string(),
        };
        let cost_budget = format!("{cost_str}/{cost_budget_str}");

        let tokens_str = format_token_metric(w.tokens.as_ref());
        let pipeline_str = w.pipeline.as_deref().unwrap_or("-");

        out.push_str(&format!(
            "{name:<name_width$} {kind_harness:<18} {owner:<6} {exit:<5} {wall_budget:<12} {cost_budget:<13} {tokens_str:<21} {pipeline_str}\n"
        ));
    }

    if !report.pipelines.is_empty() {
        let len = report.pipelines.len();
        let (slice, extra) = if len > 8 {
            (&report.pipelines[len - 8..], len - 8)
        } else {
            (&report.pipelines[..], 0)
        };
        let mut p_strs: Vec<String> = slice
            .iter()
            .map(|p| format!("{} {} r{} {}m", p.task, p.status, p.rounds, p.minutes))
            .collect();
        if extra > 0 {
            p_strs.push(format!("+{extra} more"));
        }
        out.push_str(&format!("pipelines: {}\n", p_strs.join(" · ")));
    } else {
        out.push_str("pipelines: none\n");
    }

    let unpriced_str = if report.total.unpriced == 0 {
        String::new()
    } else if report.total.unpriced == report.total.claude_unpriced {
        format!(" (+{} claude unpriced)", report.total.unpriced)
    } else {
        format!(" (+{} unpriced)", report.total.unpriced)
    };
    let total_wall_str = format_duration_metric(report.total.wall_sec);
    out.push_str(&format!(
        "total: {} workers · ${:.2}{} · {}\n",
        report.total.workers, report.total.usd, unpriced_str, total_wall_str
    ));

    // § S7: journal counts, appended so every line above stays byte-identical.
    match &report.journal_error {
        Some(err) => out.push_str(&format!("journal: unavailable - {err}\n")),
        None => {
            let j = &report.journal;
            if j.verdicts.is_empty() {
                out.push_str("verdicts: none\n");
            } else {
                let levels: Vec<String> = j
                    .verdicts
                    .iter()
                    .map(|(level, count)| format!("{level}={count}"))
                    .collect();
                out.push_str(&format!(
                    "verdicts: {} total - {}\n",
                    j.verdicts_total,
                    levels.join(" ")
                ));
            }
            if j.playbook_gaps.is_empty() {
                out.push_str("playbook_gap: none\n");
            } else {
                let per: Vec<String> = j
                    .playbook_gaps
                    .iter()
                    .map(|(name, tally)| {
                        format!(
                            "{name}={} (missing {}, unreasoned {})",
                            tally.events, tally.missing, tally.unreasoned
                        )
                    })
                    .collect();
                out.push_str(&format!(
                    "playbook_gap: {} events - {} - total missing {} unreasoned {}\n",
                    j.playbook_gap_events,
                    per.join(" - "),
                    j.playbook_gap_missing,
                    j.playbook_gap_unreasoned
                ));
            }
            let samples = if j.merges_missing_samples.is_empty() {
                String::new()
            } else {
                format!(" - {}", j.merges_missing_samples.join(", "))
            };
            let is_capped = j.truncated
                || j.merges_per_repo
                    .iter()
                    .any(|repo| repo.refs.iter().any(|r| r.truncated));
            let capped_notice = if is_capped { " (capped)" } else { "" };
            out.push_str(&format!(
                "merges: {} merge(s){} ({} incoming heads), {} without a verifier pass at the head (G1) - {} with no verdict, {} failed/blocked/inconclusive - {} repo(s), from git (no merge event is journaled), since {}{}\n",
                j.merges,
                capped_notice,
                j.merges_checked,
                j.merges_without_verdict,
                j.merges_no_verdict,
                j.merges_not_pass,
                j.merge_repos,
                report.since,
                samples
            ));
            for stat in &j.merges_per_repo {
                out.push_str(&format!(
                    "  merges {}: {} merge(s), {} head(s), {} without - refs {}\n",
                    stat.repo,
                    stat.merges,
                    stat.checked,
                    stat.without_verdict,
                    stat.refs
                        .iter()
                        .map(|r| {
                            if r.truncated {
                                format!("{}={} (capped)", r.target, r.merges)
                            } else {
                                format!("{}={}", r.target, r.merges)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            let rate = if j.accepted_reports == 0 {
                "n/a".to_string()
            } else {
                format!(
                    "{:.1}%",
                    100.0 * j.exit6_resumed as f64 / j.accepted_reports as f64
                )
            };
            out.push_str(&format!(
                "exits: {} accepted, {} exit-6, {} resumed after exit 6 (G2 {})\n",
                j.accepted_reports, j.exit6, j.exit6_resumed, rate
            ));
        }
    }

    out
}

pub fn render_metrics(runs_dir: &Path, args: &MetricsArgs) -> Result<String, String> {
    let report = read_metrics(runs_dir, args);
    if args.json {
        serde_json::to_string_pretty(&report)
            .map(|s| format!("{s}\n"))
            .map_err(|e| format!("json serialization error: {e}"))
    } else {
        Ok(render_metrics_text(&report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Child, Output};

    /// Tests scan with the embedded config so no test can change the
    /// process-global `QUAQUA_TEAM_TOML` they read.
    fn test_team_cfg() -> crate::config::Config {
        crate::config::embedded_team_config().expect("embedded team.toml")
    }

    struct MockHerdr {
        agent_list_result: std::io::Result<String>,
    }

    impl MockHerdr {
        fn new(json: impl Into<String>) -> Self {
            Self {
                agent_list_result: Ok(json.into()),
            }
        }

        fn with_err(err: std::io::Error) -> Self {
            Self {
                agent_list_result: Err(err),
            }
        }
    }

    impl HerdrOps for MockHerdr {
        fn run(&self, _args: &[&str]) -> std::io::Result<Output> {
            unimplemented!()
        }

        fn agent_list(&self) -> std::io::Result<String> {
            match &self.agent_list_result {
                Ok(s) => Ok(s.clone()),
                Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
            }
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
            _log_file: fs::File,
        ) -> std::io::Result<Child> {
            unimplemented!()
        }
    }

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("quaqua-status-{prefix}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    struct TempDirCleaner(PathBuf);
    impl Drop for TempDirCleaner {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn wf_row(id: &str, kind: &str, phase: Option<&str>, ts: u64) -> WorkflowStatusRow {
        WorkflowStatusRow {
            workflow_id: id.to_string(),
            snap_seq: None,
            last_event_kind: Some(kind.to_string()),
            last_event_seq: Some(1),
            last_event_ts_ms: Some(ts),
            phase: phase.map(str::to_string),
        }
    }

    fn vd_row(branch: &str, stale: bool, unresolved: bool, same: bool) -> BranchVerdictRow {
        BranchVerdictRow {
            branch: branch.to_string(),
            head_sha: "abcdef1234".to_string(),
            verdict_sha: "abcdef1234".to_string(),
            level: VerdictLevel::ReviewClean,
            is_verified: true,
            stale,
            unresolved,
            same_harness: same,
        }
    }

    fn filter_fixture() -> (Vec<WorkflowStatusRow>, Vec<BranchVerdictRow>) {
        let wfs = vec![
            // kept
            wf_row("k-activity", "activity_scheduled", Some("build"), 5),
            wf_row("k-child", "child_started", None, 5),
            wf_row("k-disputed", "workflow_completed", Some("disputed"), 5),
            wf_row("k-failed", "workflow_failed", None, 5),
            wf_row("k-continue", "continue", None, 5),
            wf_row("k-approval", "pause_for_approval", None, 1),
            wf_row("k-retry", "provider_retry", None, 5),
            // hidden
            wf_row("h-done", "workflow_completed", Some("completed"), 5),
            wf_row("h-done-nophase", "workflow_completed", None, 5),
            wf_row("h-cancel", "workflow_cancelled", Some("cancelled"), 5),
            wf_row("h-verdict", "verdict", None, 5),
            wf_row("h-gap", "playbook_gap", None, 5),
        ];
        let vds = vec![
            vd_row("b-ok", false, false, false),
            vd_row("b-stale", true, false, false),
            vd_row("b-same", false, false, true),
            vd_row("b-gone", false, true, false),
            vd_row("b-gone-stale", true, true, false),
        ];
        (wfs, vds)
    }

    #[test]
    fn test_status_default_hides_terminal_workflows_and_unresolved_verdicts() {
        let (wfs, vds) = filter_fixture();
        let out = format_status_report_opts(Ok(&wfs), Ok(&[]), Ok(&vds), false);
        for kept in [
            "k-activity",
            "k-child",
            "k-disputed",
            "k-failed",
            "k-continue",
            "k-approval",
            "k-retry",
            "b-ok",
            "b-stale",
            "b-same",
        ] {
            assert!(out.contains(kept), "{kept} must stay visible:\n{out}");
        }
        for hidden in [
            "h-done",
            "h-done-nophase",
            "h-cancel",
            "h-verdict",
            "h-gap",
            "b-gone",
        ] {
            assert!(!out.contains(hidden), "{hidden} must be hidden:\n{out}");
        }
        assert_eq!(
            out.matches("5 hidden (quaqua status --all)").count(),
            1,
            "{out}"
        );
        assert!(out.contains("2 hidden (quaqua status --all)"), "{out}");
    }

    #[test]
    fn test_status_all_shows_everything_without_hidden_line() {
        let (wfs, vds) = filter_fixture();
        let out = format_status_report_opts(Ok(&wfs), Ok(&[]), Ok(&vds), true);
        for id in [
            "k-approval",
            "h-done",
            "h-cancel",
            "h-verdict",
            "h-gap",
            "b-gone",
            "b-gone-stale",
        ] {
            assert!(out.contains(id), "{id} must show with --all:\n{out}");
        }
        assert!(!out.contains("hidden"), "{out}");
    }

    #[test]
    fn test_status_hidden_line_only_in_sections_that_hid_rows() {
        let wfs = vec![wf_row("k-live", "activity_scheduled", None, 5)];
        let vds = vec![vd_row("b-gone", false, true, false)];
        let out = format_status_report_opts(Ok(&wfs), Ok(&[]), Ok(&vds), false);
        assert_eq!(
            out.matches("hidden (quaqua status --all)").count(),
            1,
            "{out}"
        );
        assert!(out.contains("1 hidden (quaqua status --all)"));
        assert!(!out.contains("b-gone"));
    }

    #[test]
    fn test_status_empty_flag_cell_prints_ok() {
        let vds = vec![
            vd_row("b-ok", false, false, false),
            vd_row("b-st", true, false, false),
        ];
        let out = format_status_report_opts(Ok(&[]), Ok(&[]), Ok(&vds), true);
        let ok_line = out.lines().find(|l| l.contains("b-ok")).unwrap();
        assert!(ok_line.trim_end().ends_with(" ok"), "{ok_line}");
        let st_line = out.lines().find(|l| l.contains("b-st")).unwrap();
        assert!(st_line.trim_end().ends_with("stale"), "{st_line}");
    }

    #[test]
    fn test_status_args_all_help_and_unknown() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_status_args(&a(&[])).unwrap(),
            StatusArgs {
                all: false,
                help: false
            }
        );
        assert!(parse_status_args(&a(&["--all"])).unwrap().all);
        assert!(parse_status_args(&a(&["--help"])).unwrap().help);
        assert!(parse_status_args(&a(&["-h"])).unwrap().help);
        assert!(parse_status_args(&a(&["--bogus"])).is_err());
        assert!(parse_status_args(&a(&["--all", "extra"])).is_err());
    }

    #[test]
    fn test_status_unknown_arg_exits_2_and_help_exits_0() {
        let mock = MockHerdr::new("{}");
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(handle_status(&a(&["--bogus"]), &mock), Ok(2));
        assert_eq!(handle_status(&a(&["--help"]), &mock), Ok(0));
        assert_eq!(status_help().matches("--all").count() >= 1, true);
        for f in ["stale", "unresolved", "same-harness"] {
            assert!(status_help().contains(f), "help must document {f}");
        }
    }

    #[test]
    fn test_status_missing_db_yields_no_workflows_cleanly() {
        let tmp = temp_test_dir("missing");
        let _cleaner = TempDirCleaner(tmp.clone());

        let missing_db = tmp.join("does_not_exist.db");
        let res = query_workflows(&missing_db);
        assert!(res.is_ok(), "missing db must not error");
        let workflows = res.unwrap();
        assert!(workflows.is_empty(), "expected empty workflows");

        let report = format_status_report(Ok(&workflows), Ok(&[]), Ok(&[]));
        assert!(
            report.contains("no workflows"),
            "must contain 'no workflows'"
        );
        assert!(!missing_db.exists(), "must not create missing file");
    }

    #[test]
    fn test_status_empty_db_yields_no_workflows() {
        let tmp = temp_test_dir("empty");
        let _cleaner = TempDirCleaner(tmp.clone());

        let db_path = tmp.join("empty.db");
        {
            let conn = rusqlite::Connection::open(&db_path).expect("create db");
            conn.execute_batch(
                r#"
                CREATE TABLE events (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    ts_ms INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                CREATE TABLE snapshots (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    state_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                "#,
            )
            .expect("create schema");
        }

        let res = query_workflows(&db_path);
        assert!(res.is_ok(), "empty db must not error");
        let workflows = res.unwrap();
        assert!(workflows.is_empty(), "expected 0 workflows in empty db");

        let report = format_status_report(Ok(&workflows), Ok(&[]), Ok(&[]));
        assert!(report.contains("no workflows"));
    }

    #[test]
    fn test_status_parses_workflow_rows_from_events_and_snapshots() {
        let tmp = temp_test_dir("parsed");
        let _cleaner = TempDirCleaner(tmp.clone());

        let db_path = tmp.join("test.db");
        {
            let conn = rusqlite::Connection::open(&db_path).expect("create db");
            conn.execute_batch(
                r#"
                CREATE TABLE events (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    ts_ms INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                CREATE TABLE snapshots (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    state_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );

                INSERT INTO events VALUES ('wf-pipe-1', 1, 1000, 'workflow_started', '{}');
                INSERT INTO events VALUES ('wf-pipe-1', 2, 1020, 'activity_scheduled', '{}');
                INSERT INTO events VALUES ('wf-pipe-1', 3, 1050, 'activity_completed', '{}');
                INSERT INTO snapshots VALUES ('wf-pipe-1', 2, '{"phase": "review"}');

                INSERT INTO events VALUES ('wf-worker-2', 1, 2000, 'workflow_started', '{}');
                "#,
            )
            .expect("insert test rows");
        }

        let workflows = query_workflows(&db_path).expect("query workflows");
        assert_eq!(workflows.len(), 2, "expected 2 workflows");

        let p1 = workflows
            .iter()
            .find(|w| w.workflow_id == "wf-pipe-1")
            .expect("wf-pipe-1 found");
        assert_eq!(p1.snap_seq, Some(2));
        assert_eq!(p1.phase, Some("review".to_string()));
        assert_eq!(p1.last_event_seq, Some(3));
        assert_eq!(p1.last_event_kind, Some("activity_completed".to_string()));
        assert_eq!(p1.last_event_ts_ms, Some(1050));

        let w2 = workflows
            .iter()
            .find(|w| w.workflow_id == "wf-worker-2")
            .expect("wf-worker-2 found");
        assert_eq!(w2.snap_seq, None);
        assert_eq!(w2.phase, None);
        assert_eq!(w2.last_event_seq, Some(1));
        assert_eq!(w2.last_event_kind, Some("workflow_started".to_string()));
        assert_eq!(w2.last_event_ts_ms, Some(2000));
    }

    #[test]
    fn test_status_raises_waiter_lost_when_orchestrator_idle_and_worker_working() {
        let tmp = temp_test_dir("waiter-lost-1");
        let _cleaner = TempDirCleaner(tmp.clone());

        // mu-lead is idle; its worker sude-p4 is working
        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "mu-lead", "agent_status": "idle"},
                    {"name": "sude-p4", "agent_status": "working"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);

        fs::write(tmp.join("sude-p4.owner"), "mu-lead\n").unwrap();

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        let p4 = rows
            .iter()
            .find(|r| r.name == "sude-p4")
            .expect("sude-p4 found");
        assert!(
            p4.flags.contains(&WarningFlag::WaiterLost),
            "sude-p4 must raise WAITER-LOST because mu-lead is idle"
        );
    }

    #[test]
    fn test_status_raises_waiter_lost_when_report_exists_and_unread() {
        let tmp = temp_test_dir("waiter-lost-2");
        let _cleaner = TempDirCleaner(tmp.clone());

        // arch-lead is idle; worker sude-e1 finished and submitted report (atime <= mtime)
        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "arch-lead", "agent_status": "idle"},
                    {"name": "sude-e1", "agent_status": "done"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);

        fs::write(tmp.join("sude-e1.owner"), "arch-lead\n").unwrap();
        let report_path = tmp.join("sude-e1.md");
        fs::write(
            &report_path,
            "Goal: done\n## Blockers\nnone\n## Lessons\nnone\n",
        )
        .unwrap();

        // Explicitly set atime <= mtime (accessed before modified -> unread)
        let file = fs::File::open(&report_path).unwrap();
        let now = std::time::SystemTime::now();
        let mut times = fs::FileTimes::new();
        times = times
            .set_modified(now)
            .set_accessed(now - Duration::from_secs(5));
        file.set_times(times).unwrap();

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        let e1 = rows
            .iter()
            .find(|r| r.name == "sude-e1")
            .expect("sude-e1 found");
        assert!(
            e1.flags.contains(&WarningFlag::WaiterLost),
            "must raise WAITER-LOST when report exists unread"
        );
        assert!(
            e1.flags.contains(&WarningFlag::DoneNotCleaned),
            "must raise DONE-NOT-CLEANED"
        );
        assert_eq!(e1.report_status, ReportStatus::Unread);
    }

    #[test]
    fn test_status_negative_report_read_with_idle_owner_does_not_raise_waiter_lost() {
        let tmp = temp_test_dir("neg-report-read");
        let _cleaner = TempDirCleaner(tmp.clone());

        // arch-lead is idle; worker sude-e1 has a report that HAS been read (atime > mtime)
        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "arch-lead", "agent_status": "idle"},
                    {"name": "sude-e1", "agent_status": "done"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);

        fs::write(tmp.join("sude-e1.owner"), "arch-lead\n").unwrap();
        let report_path = tmp.join("sude-e1.md");
        fs::write(&report_path, "Goal: done\n## Lessons\nnone\n").unwrap();

        // Set atime > mtime (accessed 10s after modified -> read)
        let file = fs::File::open(&report_path).unwrap();
        let now = std::time::SystemTime::now();
        let mtime = now - Duration::from_secs(10);
        let atime = now;
        let mut times = fs::FileTimes::new();
        times = times.set_modified(mtime).set_accessed(atime);
        file.set_times(times).unwrap();

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        let e1 = rows
            .iter()
            .find(|r| r.name == "sude-e1")
            .expect("sude-e1 found");

        assert!(
            !e1.flags.contains(&WarningFlag::WaiterLost),
            "read report must NOT raise WAITER-LOST even if owner is idle"
        );
        assert_eq!(e1.report_status, ReportStatus::Present);
        assert!(e1.flags.contains(&WarningFlag::DoneNotCleaned));
    }
    #[test]
    fn test_status_read_marker_overrides_atime_unread() {
        let tmp = temp_test_dir("read-marker-override");
        let _cleaner = TempDirCleaner(tmp.clone());

        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "arch-lead", "agent_status": "idle"},
                    {"name": "sude-e1", "agent_status": "done"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);

        fs::write(tmp.join("sude-e1.owner"), "arch-lead\n").unwrap();
        let report_path = tmp.join("sude-e1.md");
        fs::write(&report_path, "Goal: done\n## Lessons\nnone\n").unwrap();

        // Force atime <= mtime (unaccessed heuristic)
        let file = fs::File::open(&report_path).unwrap();
        let now = std::time::SystemTime::now();
        let report_mtime = now - Duration::from_secs(5);
        let mut times = fs::FileTimes::new();
        times = times
            .set_modified(report_mtime)
            .set_accessed(now - Duration::from_secs(10));
        file.set_times(times).unwrap();

        // Write .read marker with mtime >= report mtime
        let marker_path = tmp.join(".sude-e1.read");
        fs::write(&marker_path, "").unwrap();
        let marker_file = fs::File::open(&marker_path).unwrap();
        let mut marker_times = fs::FileTimes::new();
        marker_times = marker_times.set_modified(now);
        marker_file.set_times(marker_times).unwrap();
        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        let e1 = rows
            .iter()
            .find(|r| r.name == "sude-e1")
            .expect("sude-e1 found");

        assert!(
            !e1.flags.contains(&WarningFlag::WaiterLost),
            ".read marker must override atime heuristic and not flag WAITER-LOST"
        );
        assert_eq!(e1.report_status, ReportStatus::Present);
    }
    #[test]
    fn test_status_stale_read_marker_treated_as_unread() {
        let tmp = temp_test_dir("stale-read-marker");
        let _cleaner = TempDirCleaner(tmp.clone());

        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "arch-lead", "agent_status": "idle"},
                    {"name": "sude-e1", "agent_status": "done"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);

        fs::write(tmp.join("sude-e1.owner"), "arch-lead\n").unwrap();
        let marker_path = tmp.join(".sude-e1.read");
        fs::write(&marker_path, "").unwrap();

        let report_path = tmp.join("sude-e1.md");
        fs::write(&report_path, "Goal: done\n## Lessons\nnone\n").unwrap();

        let now = std::time::SystemTime::now();
        let marker_file = fs::File::open(&marker_path).unwrap();
        let mut marker_times = fs::FileTimes::new();
        marker_times = marker_times.set_modified(now - Duration::from_secs(20));
        marker_file.set_times(marker_times).unwrap();

        let report_file = fs::File::open(&report_path).unwrap();
        let mut report_times = fs::FileTimes::new();
        report_times = report_times
            .set_modified(now)
            .set_accessed(now - Duration::from_secs(10));
        report_file.set_times(report_times).unwrap();

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        let e1 = rows
            .iter()
            .find(|r| r.name == "sude-e1")
            .expect("sude-e1 found");

        assert!(
            e1.flags.contains(&WarningFlag::WaiterLost),
            "stale .read marker must NOT suppress WAITER-LOST"
        );
        assert_eq!(e1.report_status, ReportStatus::Unread);
    }
    #[test]
    fn test_status_unreadable_read_marker_treated_as_unread() {
        let tmp = temp_test_dir("unreadable-read-marker");
        let _cleaner = TempDirCleaner(tmp.clone());

        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "arch-lead", "agent_status": "idle"},
                    {"name": "sude-e1", "agent_status": "done"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);

        fs::write(tmp.join("sude-e1.owner"), "arch-lead\n").unwrap();
        let report_path = tmp.join("sude-e1.md");
        fs::write(&report_path, "Goal: done\n## Lessons\nnone\n").unwrap();

        let now = std::time::SystemTime::now();
        let report_file = fs::File::open(&report_path).unwrap();
        let mut report_times = fs::FileTimes::new();
        report_times = report_times
            .set_modified(now)
            .set_accessed(now - Duration::from_secs(10));
        report_file.set_times(report_times).unwrap();

        // If read marker is a directory or otherwise unreadable for file metadata mtime
        let marker_path = tmp.join(".sude-e1.read");
        fs::create_dir(&marker_path).unwrap(); // is_file() is false, metadata/modified will not match a valid read marker

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        let e1 = rows
            .iter()
            .find(|r| r.name == "sude-e1")
            .expect("sude-e1 found");

        assert!(
            e1.flags.contains(&WarningFlag::WaiterLost),
            "unreadable or invalid .read marker must NOT suppress WAITER-LOST"
        );
        assert_eq!(e1.report_status, ReportStatus::Unread);
    }

    #[test]
    fn test_status_negative_working_owner_with_working_worker_does_not_raise_waiter_lost() {
        let tmp = temp_test_dir("neg-working");
        let _cleaner = TempDirCleaner(tmp.clone());

        // arch-lead is actively working; worker sude-e1 is working; no report
        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "arch-lead", "agent_status": "working"},
                    {"name": "sude-e1", "agent_status": "working"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);

        fs::write(tmp.join("sude-e1.owner"), "arch-lead\n").unwrap();

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        let e1 = rows
            .iter()
            .find(|r| r.name == "sude-e1")
            .expect("sude-e1 found");

        assert!(
            !e1.flags.contains(&WarningFlag::WaiterLost),
            "working worker under working owner must NOT raise WAITER-LOST"
        );
        assert!(
            e1.flags.is_empty(),
            "expected 0 flags on healthy working worker"
        );
    }

    #[test]
    fn test_status_corrupted_or_unreadable_db_surfaces_db_error() {
        let tmp = temp_test_dir("corrupt");
        let _cleaner = TempDirCleaner(tmp.clone());

        let db_path = tmp.join("corrupt.db");
        fs::write(&db_path, b"not a sqlite database file at all").unwrap();

        let res = query_workflows(&db_path);
        assert!(res.is_err(), "corrupt db must return Err");
        let err_msg = res.unwrap_err();
        assert!(
            err_msg.starts_with("db error:"),
            "error message must start with 'db error:' but got: {err_msg}"
        );

        let report = format_status_report(Err(&err_msg), Ok(&[]), Ok(&[]));
        assert!(
            report.contains("db error:"),
            "rendered report must show db error"
        );
        assert!(
            !report.contains("no workflows"),
            "rendered report must NOT claim 'no workflows' when DB is corrupted"
        );
    }

    #[test]
    fn test_status_schema_drifted_undecodable_row_surfaces_db_error() {
        let tmp = temp_test_dir("schema-drift");
        let _cleaner = TempDirCleaner(tmp.clone());

        let db_path = tmp.join("drift.db");
        {
            let conn = rusqlite::Connection::open(&db_path).expect("create db");
            conn.execute_batch(
                r#"
                CREATE TABLE events (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    ts_ms TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                INSERT INTO events VALUES ('wf-drift', 1, 'not_an_integer_timestamp', 'kind', '{}');
                "#,
            )
            .expect("init drift schema");
        }

        let res = query_workflows(&db_path);
        assert!(
            res.is_err(),
            "schema drift / decoding failure must return Err"
        );
        let err_msg = res.unwrap_err();
        assert!(
            err_msg.contains("db error: decoding event row"),
            "must report decoding error, got: {err_msg}"
        );
    }

    #[test]
    fn test_status_herdr_agent_list_error_surfaces_herdr_error() {
        let tmp = temp_test_dir("herdr-err");
        let _cleaner = TempDirCleaner(tmp.clone());

        let io_err = std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "herdr socket unavailable",
        );
        let mock = MockHerdr::with_err(io_err);

        // Even with an owner file present, it must NOT emit false ORPHAN rows!
        fs::write(tmp.join("sude-test.owner"), "arch-lead\n").unwrap();

        let res = scan_orchestration(&tmp, &mock, &test_team_cfg());
        assert!(
            res.is_err(),
            "scan_orchestration must fail when herdr fails"
        );
        let err_msg = res.unwrap_err();
        assert!(
            err_msg.starts_with("herdr error:"),
            "error must start with 'herdr error:' but got: {err_msg}"
        );

        let report = format_status_report(Ok(&[]), Err(&err_msg), Ok(&[]));
        assert!(
            report.contains("herdr error:"),
            "report must show herdr error"
        );
        assert!(
            !report.contains("ORPHAN"),
            "must NOT emit false ORPHAN flags when herdr is unreachable"
        );
    }

    #[test]
    fn test_status_herdr_invalid_json_surfaces_herdr_error() {
        let tmp = temp_test_dir("herdr-json-err");
        let _cleaner = TempDirCleaner(tmp.clone());

        let mock = MockHerdr::new("invalid json output from herdr");
        let res = scan_orchestration(&tmp, &mock, &test_team_cfg());
        assert!(
            res.is_err(),
            "scan_orchestration must fail on unparseable JSON"
        );
        let err_msg = res.unwrap_err();
        assert!(
            err_msg.contains("herdr error: invalid agent list JSON"),
            "error must identify invalid json, got: {err_msg}"
        );

        let report = format_status_report(Ok(&[]), Err(&err_msg), Ok(&[]));
        assert!(report.contains("herdr error: invalid agent list JSON"));
    }

    #[test]
    fn test_status_flags_done_not_cleaned_idle_unprompted_and_orphan() {
        let tmp = temp_test_dir("flags");
        let _cleaner = TempDirCleaner(tmp.clone());

        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "active-lead", "agent_status": "working"},
                    {"name": "sude-done", "agent_status": "done"},
                    {"name": "sude-idle", "agent_status": "idle"},
                    {"name": "sude-orphan", "agent_status": "working"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);

        fs::write(tmp.join("sude-done.owner"), "active-lead\n").unwrap();
        fs::write(tmp.join("sude-idle.owner"), "active-lead\n").unwrap();
        fs::write(tmp.join("sude-orphan.owner"), "ghost-lead\n").unwrap();

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");

        let done = rows
            .iter()
            .find(|r| r.name == "sude-done")
            .expect("sude-done found");
        assert!(done.flags.contains(&WarningFlag::DoneNotCleaned));

        let idle = rows
            .iter()
            .find(|r| r.name == "sude-idle")
            .expect("sude-idle found");
        assert!(idle.flags.contains(&WarningFlag::IdleUnprompted));

        let orphan = rows
            .iter()
            .find(|r| r.name == "sude-orphan")
            .expect("sude-orphan found");
        assert!(orphan.flags.contains(&WarningFlag::Orphan));
        assert!(orphan.flags.contains(&WarningFlag::WaiterLost));
    }

    #[test]
    fn test_status_table_formatting_text_output() {
        let _env_lock = crate::test_env::lock_env();
        let tmp = temp_test_dir("format");
        let _cleaner = TempDirCleaner(tmp.clone());

        let db_path = tmp.join("test.db");
        {
            let conn = rusqlite::Connection::open(&db_path).expect("create db");
            conn.execute_batch(
                r#"
                CREATE TABLE events (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    ts_ms INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                CREATE TABLE snapshots (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    state_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                INSERT INTO events VALUES ('pipe-test', 1, 1000, 'workflow_started', '{}');
                INSERT INTO snapshots VALUES ('pipe-test', 1, '{"phase":"build"}');
                "#,
            )
            .expect("init rows");
        }

        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "arch-lead", "agent_status": "working"},
                    {"name": "sude-w1", "agent_status": "working"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);
        fs::write(tmp.join("sude-w1.owner"), "arch-lead\n").unwrap();

        let out = render_status(&db_path, &tmp, &mock).expect("render status");
        assert!(out.contains("=== Workflows (runs/quaqua.db) ==="));
        assert!(out.contains("pipe-test"));
        assert!(out.contains("=== Orchestration ==="));
        assert!(out.contains("arch-lead"));
        assert!(out.contains("sude-w1"));
        assert!(out.contains("WAIT"));
        assert!(out.contains("TEAM"));
    }
    #[test]
    fn test_status_orchestration_shows_estimate_when_group_has_at_least_min_samples() {
        let _env_lock = crate::test_env::lock_env();
        let tmp = temp_test_dir("est-min-samples");
        let _cleaner = TempDirCleaner(tmp.clone());

        let db_path = tmp.join("quaqua.db");
        {
            let conn = rusqlite::Connection::open(&db_path).expect("create db");
            conn.execute_batch(
                r#"
                CREATE TABLE events (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    ts_ms INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                CREATE TABLE snapshots (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    state_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                "#,
            )
            .expect("init tables");

            // Insert 5 successful runs: 100s, 200s, 300s, 400s, 500s -> median 300s (5m)
            for (i, dur_ms) in [100_000, 200_000, 300_000, 400_000, 500_000]
                .iter()
                .enumerate()
            {
                let wf = format!("worker:sude-p{i}#1");
                conn.execute(
                    "INSERT INTO events VALUES (?, 10, 1000, 'workflow_completed', '{\"exit\":0}');",
                    [&wf],
                )
                .unwrap();
                let state = format!(
                    "{{\"input\":{{\"kind\":\"sude\"}},\"config\":{{\"playbook\":{{\"name\":\"feature\"}}}},\"prev_elapsed_ms\":{dur_ms}}}"
                );
                conn.execute("INSERT INTO snapshots VALUES (?, 10, ?);", [&wf, &state])
                    .unwrap();
            }

            // Also insert an exit=3 (failed) run that should be ignored
            conn.execute(
                "INSERT INTO events VALUES ('worker:sude-bad#1', 10, 2000, 'workflow_completed', '{\"exit\":3}');",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO snapshots VALUES ('worker:sude-bad#1', 10, '{\"input\":{\"kind\":\"sude\"},\"config\":{\"playbook\":{\"name\":\"feature\"}},\"prev_elapsed_ms\":10000}');",
                [],
            )
            .unwrap();
        }

        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "sude-w1", "agent_status": "working"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);
        fs::write(tmp.join("sude-w1.owner"), "cos\n").unwrap();
        // Playbook pin for sude-w1: feature
        fs::write(
            tmp.join(".sude-w1.playbook"),
            "name=feature\nids=1,2,3,4,5\n",
        )
        .unwrap();
        // Elapsed: 60s (1m)
        fs::write(tmp.join(".sude-w1.elapsed"), "60\n").unwrap();

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.name, "sude-w1");
        assert!(row.estimate.is_some(), "must have estimate with 5 samples");
        let est = row.estimate.as_ref().unwrap();
        assert_eq!(est.sample_size, 5);
        assert_eq!(est.median_duration, std::time::Duration::from_secs(300));
        assert_eq!(est.time_left, std::time::Duration::from_secs(240));
        // wait_time formatted with estimate: 1m (~4m left, n=5)
        assert_eq!(row.wait_time, "1m (~4m left, n=5)");

        let out = format_status_report(Ok(&[]), Ok(&rows), Ok(&[]));
        assert!(out.contains("sude-w1"));
        assert!(out.contains("1m (~4m left, n=5)"));
    }

    #[test]
    fn test_status_orchestration_no_estimate_when_fewer_than_min_samples() {
        let _env_lock = crate::test_env::lock_env();
        let tmp = temp_test_dir("est-few-samples");
        let _cleaner = TempDirCleaner(tmp.clone());

        let db_path = tmp.join("quaqua.db");
        {
            let conn = rusqlite::Connection::open(&db_path).expect("create db");
            conn.execute_batch(
                r#"
                CREATE TABLE events (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    ts_ms INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                CREATE TABLE snapshots (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    state_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                "#,
            )
            .expect("init tables");

            // Insert only 4 successful runs (N=5 required)
            for (i, dur_ms) in [100_000, 200_000, 300_000, 400_000].iter().enumerate() {
                let wf = format!("worker:sude-p{i}#1");
                conn.execute(
                    "INSERT INTO events VALUES (?, 10, 1000, 'workflow_completed', '{\"exit\":0}');",
                    [&wf],
                )
                .unwrap();
                let state = format!(
                    "{{\"input\":{{\"kind\":\"sude\"}},\"config\":{{\"playbook\":{{\"name\":\"bug-fix\"}}}},\"prev_elapsed_ms\":{dur_ms}}}"
                );
                conn.execute("INSERT INTO snapshots VALUES (?, 10, ?);", [&wf, &state])
                    .unwrap();
            }
        }

        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "sude-w1", "agent_status": "working"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);
        fs::write(tmp.join("sude-w1.owner"), "cos\n").unwrap();
        fs::write(
            tmp.join(".sude-w1.playbook"),
            "name=bug-fix\nids=1,2,3,4,5\n",
        )
        .unwrap();
        fs::write(tmp.join(".sude-w1.elapsed"), "60\n").unwrap();

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.estimate, None);
        assert_eq!(row.wait_time, "1m");
    }

    #[test]
    fn test_status_orchestration_no_estimate_for_idle_or_done_worker() {
        let _env_lock = crate::test_env::lock_env();
        let tmp = temp_test_dir("est-idle-worker");
        let _cleaner = TempDirCleaner(tmp.clone());

        let db_path = tmp.join("quaqua.db");
        {
            let conn = rusqlite::Connection::open(&db_path).expect("create db");
            conn.execute_batch(
                r#"
                CREATE TABLE events (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    ts_ms INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                CREATE TABLE snapshots (
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    state_json TEXT NOT NULL,
                    PRIMARY KEY (workflow_id, seq)
                );
                "#,
            )
            .expect("init tables");

            for (i, dur_ms) in [100_000, 200_000, 300_000, 400_000, 500_000]
                .iter()
                .enumerate()
            {
                let wf = format!("worker:sude-p{i}#1");
                conn.execute(
                    "INSERT INTO events VALUES (?, 10, 1000, 'workflow_completed', '{\"exit\":0}');",
                    [&wf],
                )
                .unwrap();
                let state = format!(
                    "{{\"input\":{{\"kind\":\"sude\"}},\"config\":{{\"playbook\":{{\"name\":\"feature\"}}}},\"prev_elapsed_ms\":{dur_ms}}}"
                );
                conn.execute("INSERT INTO snapshots VALUES (?, 10, ?);", [&wf, &state])
                    .unwrap();
            }
        }

        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "sude-w1", "agent_status": "idle"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);
        fs::write(tmp.join("sude-w1.owner"), "cos\n").unwrap();
        fs::write(
            tmp.join(".sude-w1.playbook"),
            "name=feature\nids=1,2,3,4,5\n",
        )
        .unwrap();
        fs::write(tmp.join(".sude-w1.elapsed"), "60\n").unwrap();

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].estimate, None);
        assert_eq!(rows[0].wait_time, "1m");
    }

    #[test]
    fn test_status_orchestration_team_invariant() {
        let tmp = temp_test_dir("team-invariant");
        let _cleaner = TempDirCleaner(tmp.clone());

        // 1. peer -> lead slug
        fs::write(tmp.join("sude-peer.owner"), "gate-lead\n").unwrap();

        // 2. lead -> own slug (even without owner file or with owner=cos)
        fs::write(tmp.join("gate-lead.owner"), "cos\n").unwrap();

        // 3. cos-owned -> cos
        fs::write(tmp.join("sude-cos.owner"), "cos\n").unwrap();

        // 4. cycle -> ?
        fs::write(tmp.join("sude-c1.owner"), "sude-c2\n").unwrap();
        fs::write(tmp.join("sude-c2.owner"), "sude-c1\n").unwrap();

        // 5. missing -> ? (in live agents, no .owner file)

        let mock_json = r#"{
            "result": {
                "agents": [
                    {"name": "gate-lead", "agent_status": "working"},
                    {"name": "sude-peer", "agent_status": "working"},
                    {"name": "sude-cos", "agent_status": "working"},
                    {"name": "sude-c1", "agent_status": "working"},
                    {"name": "sude-missing", "agent_status": "working"}
                ]
            }
        }"#;
        let mock = MockHerdr::new(mock_json);

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");

        let find_team = |name: &str| -> String {
            rows.iter()
                .find(|r| r.name == name)
                .map(|r| r.team.clone())
                .unwrap_or_else(|| panic!("row '{name}' not found"))
        };

        // 1. peer -> lead slug
        assert_eq!(find_team("sude-peer"), "gate");
        // 2. lead -> own slug
        assert_eq!(find_team("gate-lead"), "gate");
        // 3. cos-owned -> cos
        assert_eq!(find_team("sude-cos"), "cos");
        // 4. cycle -> ?
        assert_eq!(find_team("sude-c1"), "?");
        // 5. missing -> ?

        // Multi-hop: worker -> worker -> lead
        fs::write(tmp.join("sude-sub.owner"), "sude-peer\n").unwrap();
        assert_eq!(resolve_team(&tmp, "sude-sub"), "gate");

        // Multi-hop: worker -> worker -> cos
        fs::write(tmp.join("sude-subcos.owner"), "sude-cos\n").unwrap();
        assert_eq!(resolve_team(&tmp, "sude-subcos"), "cos");

        // Empty / nonexistent
        assert_eq!(resolve_team(&tmp, ""), "?");
        assert_eq!(resolve_team(&tmp, "sude-missing"), "?");

        // Table format includes TEAM column
        let report = format_status_report(Ok(&[]), Ok(&rows), Ok(&[]));
        assert!(report.contains("TEAM"));
        assert_eq!(find_team("sude-missing"), "?");
    }

    #[test]
    fn test_status_rows_take_status_from_state_file_with_screen_fallback() {
        let tmp = temp_test_dir("status-hook-state");
        let _cleaner = TempDirCleaner(tmp.clone());
        let now = crate::util::now_sec();

        let mock = MockHerdr::new(
            r#"{
            "result": {
                "agents": [
                    {"name": "gate-lead", "agent_status": "idle"},
                    {"name": "sude-h1", "agent_status": "idle"},
                    {"name": "sude-h2", "agent_status": "working"},
                    {"name": "sude-h3", "agent_status": "done"},
                    {"name": "sude-h4", "agent_status": "working"}
                ]
            }
        }"#,
        );
        for worker in ["sude-h1", "sude-h2", "sude-h3", "sude-h4"] {
            let owner = if worker == "sude-h1" {
                "gate-lead"
            } else {
                "cos"
            };
            fs::write(tmp.join(format!("{worker}.owner")), format!("{owner}\n")).unwrap();
        }
        // h1: screen says idle, the hook says working -> working, and its idle
        // lead still raises WAITER-LOST (flags follow the resolved state).
        fs::write(tmp.join(".sude-h1.state"), format!("working {now}\n")).unwrap();
        // h2: screen says working, the hook says blocked -> blocked.
        fs::write(tmp.join(".sude-h2.state"), format!("blocked {now}\n")).unwrap();
        // h3: no state file -> screen ("done").
        // h4: state file older than the staleness bound -> screen ("working").
        fs::write(
            tmp.join(".sude-h4.state"),
            format!(
                "blocked {}\n",
                now.saturating_sub(crate::liveness::DEFAULT_STATE_STALENESS_SEC + 60)
            ),
        )
        .unwrap();

        let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).expect("scan orchestration");
        let row = |name: &str| {
            rows.iter()
                .find(|r| r.name == name)
                .unwrap_or_else(|| panic!("row '{name}' not found"))
                .clone()
        };

        assert_eq!(row("sude-h1").agent_status, "working");
        assert_eq!(row("sude-h2").agent_status, "blocked");
        assert_eq!(row("sude-h3").agent_status, "done");
        assert_eq!(row("sude-h4").agent_status, "working");

        // #179: a blocked row carries exactly one standard reason; others none.
        // h2's hook gave no reason -> generic fallback.
        assert_eq!(row("sude-h2").blocked_reason, Some(BlockReason::NeedsInput));
        assert_eq!(row("sude-h1").blocked_reason, None);
        assert_eq!(row("sude-h4").blocked_reason, None);
        for (token, want) in [
            ("plan", BlockReason::PlanToApprove),
            ("permission", BlockReason::AsksPermission),
            ("signed-out", BlockReason::SignedOut),
            ("question", BlockReason::QuestionForYou),
            ("garbage", BlockReason::NeedsInput),
        ] {
            fs::write(
                tmp.join(".sude-h2.state"),
                format!("blocked {now} {token}\n"),
            )
            .unwrap();
            let rows = scan_orchestration(&tmp, &mock, &test_team_cfg()).unwrap();
            let r = rows.iter().find(|r| r.name == "sude-h2").unwrap();
            assert_eq!(r.agent_status, "blocked");
            assert_eq!(r.blocked_reason, Some(want), "token {token}");
            assert!(want.label().len() <= 40);
            let out = format_status_report(Ok(&[]), Ok(&rows), Ok(&[]));
            let line = out.lines().find(|l| l.starts_with("sude-h2")).unwrap();
            assert!(line.contains(want.label()), "{line}");
        }

        // Flags read the resolved status: h1 is working while its lead is idle.
        assert!(
            row("sude-h1").flags.contains(&WarningFlag::WaiterLost),
            "WAITER-LOST must follow the hook-reported working state: {:?}",
            row("sude-h1").flags
        );
        // h2 is not working any more, so nothing is left waiting on it.
        assert!(
            !row("sude-h2").flags.contains(&WarningFlag::WaiterLost),
            "a blocked worker raises no WAITER-LOST: {:?}",
            row("sude-h2").flags
        );
    }

    #[test]
    fn test_status_renders_all_sections_when_team_config_fails_to_load() {
        let tmp = temp_test_dir("bad-team-config");
        let _cleaner = TempDirCleaner(tmp.clone());
        let db_path = tmp.join("quaqua.db");
        let mock = MockHerdr::new(r#"{"result":{"agents":[]}}"#);

        // A broken explicit team.toml must surface in the orchestration section;
        // the workflows and verdicts sections still have to render.
        let missing = tmp.join("missing-team.toml");
        let _env_lock = crate::test_env::lock_env();
        let _guard = EnvGuard::capture(&["QUAQUA_TEAM_TOML"]);
        std::env::set_var("QUAQUA_TEAM_TOML", &missing);

        let out = render_status(&db_path, &tmp, &mock)
            .expect("status must keep rendering when team.toml fails to load");
        assert!(out.contains("=== Workflows (runs/quaqua.db) ==="), "{out}");
        assert!(out.contains("=== Verdicts ==="), "{out}");
        assert!(out.contains("=== Orchestration ==="), "{out}");
        assert!(
            out.contains("missing-team.toml"),
            "orchestration section must show the team.toml error: {out}"
        );
    }

    #[test]
    fn test_status_verdict_stale_on_new_commit() {
        let tmp = temp_test_dir("stale-commit");
        let _cleaner = TempDirCleaner(tmp.clone());
        let repo = tmp.join("repo");
        fs::create_dir_all(&repo).unwrap();

        // Init git repo
        let git = Git::new();
        let _ = git.run_in(&repo, &["init", "-q"]).unwrap();
        let _ = git
            .run_in(&repo, &["config", "user.email", "test@test"])
            .unwrap();
        let _ = git.run_in(&repo, &["config", "user.name", "test"]).unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "initial commit"])
            .unwrap();

        // Create branch quaqua/feat-stale at commit 1
        let _ = git
            .run_in(&repo, &["checkout", "-qb", "quaqua/feat-stale"])
            .unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "commit 1"])
            .unwrap();
        let sha1 =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();

        // Record verdict in journal at sha1
        let db_path = tmp.join("quaqua.db");
        let mut journal = SqliteJournal::open(&db_path).expect("open journal");
        let v1 = VerdictRecord {
            branch: "quaqua/feat-stale".to_string(),
            sha: sha1.clone(),
            level: VerdictLevel::ReviewClean,
            evidence: "verdict evidence".to_string(),
            verifier: "suhuynh-rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("suhuynh".to_string()),
            harness: "pi".to_string(),
        };
        journal
            .record_verdict("wf-stale", &v1)
            .expect("record verdict");

        // Before new commit: head == verdict SHA -> not stale
        let rows_before = query_branch_verdicts(&db_path, &repo, &git).expect("query before");
        assert_eq!(rows_before.len(), 1);
        assert_eq!(rows_before[0].branch, "quaqua/feat-stale");
        assert_eq!(rows_before[0].head_sha, sha1);
        assert_eq!(rows_before[0].verdict_sha, sha1);
        assert_eq!(rows_before[0].level, VerdictLevel::ReviewClean);
        assert!(!rows_before[0].stale);

        // Make new commit on the branch
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "commit 2 (new)"])
            .unwrap();
        let sha2 =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();
        assert_ne!(sha1, sha2, "new commit must produce different SHA");

        // Query branch verdicts after new commit
        let rows_after = query_branch_verdicts(&db_path, &repo, &git).expect("query after");
        assert_eq!(rows_after.len(), 1);
        assert_eq!(rows_after[0].branch, "quaqua/feat-stale");
        assert_eq!(rows_after[0].head_sha, sha2);
        assert_eq!(rows_after[0].verdict_sha, sha1);
        assert_eq!(rows_after[0].level, VerdictLevel::ReviewClean);
        assert!(
            rows_after[0].stale,
            "verdict must be marked stale on new commit"
        );

        // Check format_status_report output contains stale flag
        let report = format_status_report(Ok(&[]), Ok(&[]), Ok(&rows_after));
        assert!(report.contains("quaqua/feat-stale"));
        assert!(report.contains("review-clean"));
        assert!(report.contains("stale"));
    }
    #[test]
    fn test_render_status_with_git_resolves_repo_from_index_and_flags_stale_or_unresolved() {
        let _env_lock = crate::test_env::lock_env();
        let ws = temp_test_dir("ws-render-status");
        let _cleaner_ws = TempDirCleaner(ws.clone());
        let proj_repo = temp_test_dir("proj-repo");
        let _cleaner_repo = TempDirCleaner(proj_repo.clone());

        fs::create_dir_all(&proj_repo).unwrap();
        let git = Git::new();
        let _ = git.run_in(&proj_repo, &["init", "-q"]).unwrap();
        let _ = git
            .run_in(&proj_repo, &["config", "user.email", "test@test"])
            .unwrap();
        let _ = git
            .run_in(&proj_repo, &["config", "user.name", "test"])
            .unwrap();
        let _ = git
            .run_in(&proj_repo, &["commit", "--allow-empty", "-qm", "initial"])
            .unwrap();

        // Branch quaqua/feat-moved in proj_repo
        let _ = git
            .run_in(&proj_repo, &["checkout", "-qb", "quaqua/feat-moved"])
            .unwrap();
        let _ = git
            .run_in(&proj_repo, &["commit", "--allow-empty", "-qm", "commit 1"])
            .unwrap();
        let sha1 = String::from_utf8_lossy(
            &git.run_in(&proj_repo, &["rev-parse", "HEAD"])
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();

        // Setup workspace (NOT a git repository)
        let projects_dir = ws.join("projects");
        fs::create_dir_all(&projects_dir).unwrap();
        fs::write(
            projects_dir.join("INDEX.md"),
            format!(
                "# Projects\n\n| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| myproj | {} | cos | active | test |\n",
                proj_repo.display()
            ),
        )
        .unwrap();

        let runs_dir = ws.join("runs");
        fs::create_dir_all(&runs_dir).unwrap();
        let db_path = runs_dir.join("quaqua.db");
        let mut journal = SqliteJournal::open(&db_path).expect("open journal");

        let v_stale = VerdictRecord {
            branch: "quaqua/feat-moved".to_string(),
            sha: sha1.clone(),
            level: VerdictLevel::ReviewClean,
            evidence: "evidence".to_string(),
            verifier: "suhuynh-rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("suhuynh".to_string()),
            harness: "pi".to_string(),
        };
        journal.record_verdict("wf-1", &v_stale).unwrap();

        let v_unresolved = VerdictRecord {
            branch: "quaqua/feat-missing".to_string(),
            sha: "999999999999".to_string(),
            level: VerdictLevel::UnitTestVerified,
            evidence: "evidence".to_string(),
            verifier: "suhuynh-rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("suhuynh".to_string()),
            harness: "pi".to_string(),
        };
        journal.record_verdict("wf-2", &v_unresolved).unwrap();

        // Advance quaqua/feat-moved in proj_repo
        let _ = git
            .run_in(&proj_repo, &["commit", "--allow-empty", "-qm", "commit 2"])
            .unwrap();
        let sha2 = String::from_utf8_lossy(
            &git.run_in(&proj_repo, &["rev-parse", "HEAD"])
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        assert_ne!(sha1, sha2);

        let mock = MockHerdr::new(r#"{"result":{"agents":[]}}"#);
        let out = render_status_with_git(&db_path, &runs_dir, &mock, &git)
            .expect("render status with git");

        let stale_lines: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("quaqua/feat-moved"))
            .collect();
        assert_eq!(stale_lines.len(), 1);
        assert!(
            stale_lines[0].contains("stale"),
            "moved branch must be flagged stale when resolved from INDEX.md, got: {}",
            stale_lines[0]
        );

        let unres_lines: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("quaqua/feat-missing"))
            .collect();
        assert_eq!(unres_lines.len(), 1);
        assert!(
            unres_lines[0].contains("unresolved"),
            "unresolvable branch must be flagged unresolved, got: {}",
            unres_lines[0]
        );
    }

    #[test]
    fn test_status_verdict_strongest_per_branch_and_sha() {
        let tmp = temp_test_dir("strongest-sha");
        let _cleaner = TempDirCleaner(tmp.clone());
        let repo = tmp.join("repo");
        fs::create_dir_all(&repo).unwrap();

        let git = Git::new();
        let _ = git.run_in(&repo, &["init", "-q"]).unwrap();
        let _ = git
            .run_in(&repo, &["config", "user.email", "test@test"])
            .unwrap();
        let _ = git.run_in(&repo, &["config", "user.name", "test"]).unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "initial"])
            .unwrap();

        let _ = git
            .run_in(&repo, &["checkout", "-qb", "quaqua/feat-strong"])
            .unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "work"])
            .unwrap();
        let sha =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();

        let db_path = tmp.join("quaqua.db");
        let mut journal = SqliteJournal::open(&db_path).expect("open journal");

        // 1. Builder self-report
        let v_builder = VerdictRecord {
            branch: "quaqua/feat-strong".to_string(),
            sha: sha.clone(),
            level: VerdictLevel::UnitTestVerified,
            evidence: "builder unit tests pass".to_string(),
            verifier: "sude-build".to_string(),
            source: VerdictSource::SelfReport,
            builder_kind: Some("sude".to_string()),
            verifier_kind: None,
            harness: "omp".to_string(),
        };
        journal.record_verdict("wf-builder", &v_builder).unwrap();

        // 2. Verifier verdict (review-clean overrides self-report)
        let v_rev_clean = VerdictRecord {
            branch: "quaqua/feat-strong".to_string(),
            sha: sha.clone(),
            level: VerdictLevel::ReviewClean,
            evidence: "Verdict: clean".to_string(),
            verifier: "suhuynh-rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("suhuynh".to_string()),
            harness: "pi".to_string(),
        };
        journal
            .record_verdict("wf-rev-clean", &v_rev_clean)
            .unwrap();

        let rows = query_branch_verdicts(&db_path, &repo, &git).expect("query verdicts");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].branch, "quaqua/feat-strong");
        assert_eq!(rows[0].level, VerdictLevel::ReviewClean);
        assert!(rows[0].is_verified);

        // 3. Negative dominates: second verifier records verifier-blocked
        let v_rev_blocked = VerdictRecord {
            branch: "quaqua/feat-strong".to_string(),
            sha: sha.clone(),
            level: VerdictLevel::VerifierBlocked,
            evidence: "Verdict: 1 findings".to_string(),
            verifier: "truonglao-rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("truonglao".to_string()),
            harness: "claude".to_string(),
        };
        journal
            .record_verdict("wf-rev-blocked", &v_rev_blocked)
            .unwrap();

        let rows2 = query_branch_verdicts(&db_path, &repo, &git).expect("query verdicts 2");
        assert_eq!(rows2.len(), 1);
        assert_eq!(
            rows2[0].level,
            VerdictLevel::VerifierBlocked,
            "verifier-blocked must dominate over review-clean"
        );
        assert!(!rows2[0].is_verified);

        let report = format_status_report(Ok(&[]), Ok(&[]), Ok(&rows2));
        assert!(report.contains("quaqua/feat-strong"));
        assert!(report.contains("verifier-blocked"));
    }

    #[test]
    fn test_status_verdict_self_report_at_head_is_inconclusive() {
        let tmp = temp_test_dir("self-report-head");
        let _cleaner = TempDirCleaner(tmp.clone());
        let repo = tmp.join("repo");
        fs::create_dir_all(&repo).unwrap();

        let git = Git::new();
        let _ = git.run_in(&repo, &["init", "-q"]).unwrap();
        let _ = git
            .run_in(&repo, &["config", "user.email", "test@test"])
            .unwrap();
        let _ = git.run_in(&repo, &["config", "user.name", "test"]).unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "initial"])
            .unwrap();

        let _ = git
            .run_in(&repo, &["checkout", "-qb", "quaqua/feat-self"])
            .unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "work"])
            .unwrap();
        let sha =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();

        let db_path = tmp.join("quaqua.db");
        let mut journal = SqliteJournal::open(&db_path).expect("open journal");

        // The only verdict for this branch is the builder's own report at head.
        let v_builder = VerdictRecord {
            branch: "quaqua/feat-self".to_string(),
            sha: sha.clone(),
            level: VerdictLevel::UnitTestVerified,
            evidence: "builder unit tests pass".to_string(),
            verifier: "sude-build".to_string(),
            source: VerdictSource::SelfReport,
            builder_kind: Some("sude".to_string()),
            verifier_kind: None,
            harness: "omp".to_string(),
        };
        journal.record_verdict("wf-builder", &v_builder).unwrap();

        // The selector `quaqua status` uses must apply S1's F6 rule itself.
        let records = vec![&v_builder];
        let best = strongest_verdict_from_slice(&records).expect("verdict");
        assert_eq!(
            best.level,
            VerdictLevel::Inconclusive,
            "self-report alone must select as inconclusive, never its self-reported level"
        );
        assert!(!best.is_verified());

        let rows = query_branch_verdicts(&db_path, &repo, &git).expect("query verdicts");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].level, VerdictLevel::Inconclusive);
        assert!(!rows[0].is_verified);

        let report = format_status_report(Ok(&[]), Ok(&[]), Ok(&rows));
        assert!(report.contains("quaqua/feat-self"));
        assert!(report.contains("inconclusive"));
        assert!(!report.contains("unit-test-verified"));
    }

    #[test]
    fn test_status_verdict_same_harness_shown() {
        let tmp = temp_test_dir("same-harness");
        let _cleaner = TempDirCleaner(tmp.clone());
        let repo = tmp.join("repo");
        fs::create_dir_all(&repo).unwrap();

        // Pin QUAQUA_TEAM_TOML to a fixture config where builder and verifier differ on exactly one side (§ S2 fix)
        let fixture_path = tmp.join("fixture_team.toml");
        fs::write(
            &fixture_path,
            r#"schema_version = 1

[kinds.k-builder]
harness = "omp"
model = "m1"

[kinds.k-verifier-same]
harness = "omp"
model = "m2"

[kinds.k-verifier-diff]
harness = "pi"
model = "m3"
"#,
        )
        .unwrap();

        let _env_lock = crate::test_env::lock_env();
        let _env_guard = EnvGuard::capture(&["QUAQUA_TEAM_TOML"]);
        std::env::set_var("QUAQUA_TEAM_TOML", &fixture_path);
        let git = Git::new();
        let _ = git.run_in(&repo, &["init", "-q"]).unwrap();
        let _ = git
            .run_in(&repo, &["config", "user.email", "test@test"])
            .unwrap();
        let _ = git.run_in(&repo, &["config", "user.name", "test"]).unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "initial"])
            .unwrap();

        // Branch 1: detu (verifier, omp) verifying sude (builder, omp) -> same-harness
        let _ = git
            .run_in(&repo, &["checkout", "-qb", "quaqua/feat-same"])
            .unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "work same"])
            .unwrap();
        let sha_same =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();

        // Branch 2: suhuynh (verifier, pi) verifying sude (builder, omp) -> diff harness
        let _ = git
            .run_in(&repo, &["checkout", "-qb", "quaqua/feat-diff"])
            .unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "work diff"])
            .unwrap();
        let sha_diff =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();

        let db_path = tmp.join("quaqua.db");
        let mut journal = SqliteJournal::open(&db_path).expect("open journal");

        let v_same = VerdictRecord {
            branch: "quaqua/feat-same".to_string(),
            sha: sha_same,
            level: VerdictLevel::ReviewClean,
            evidence: "clean".to_string(),
            verifier: "detu-rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("k-builder".to_string()),
            verifier_kind: Some("k-verifier-same".to_string()),
            harness: "omp".to_string(),
        };
        journal.record_verdict("wf-same", &v_same).unwrap();

        let v_diff = VerdictRecord {
            branch: "quaqua/feat-diff".to_string(),
            sha: sha_diff,
            level: VerdictLevel::ReviewClean,
            evidence: "clean".to_string(),
            verifier: "suhuynh-rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("k-builder".to_string()),
            verifier_kind: Some("k-verifier-diff".to_string()),
            harness: "pi".to_string(),
        };
        journal.record_verdict("wf-diff", &v_diff).unwrap();

        let rows = query_branch_verdicts(&db_path, &repo, &git).expect("query verdicts");
        assert_eq!(rows.len(), 2);

        let row_diff = rows
            .iter()
            .find(|r| r.branch == "quaqua/feat-diff")
            .unwrap();
        assert!(!row_diff.same_harness);

        let row_same = rows
            .iter()
            .find(|r| r.branch == "quaqua/feat-same")
            .unwrap();
        assert!(
            row_same.same_harness,
            "must show same-harness when harnesses match"
        );

        let report = format_status_report(Ok(&[]), Ok(&[]), Ok(&rows));
        assert!(report.contains("same-harness"));
        let lines: Vec<&str> = report
            .lines()
            .filter(|l| l.contains("quaqua/feat-same"))
            .collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("same-harness"));
    }
    #[test]
    fn test_status_verdicts_grouped_in_one_pass() {
        let tmp = temp_test_dir("group-one-pass");
        let _cleaner = TempDirCleaner(tmp.clone());
        let repo = tmp.join("repo");
        fs::create_dir_all(&repo).unwrap();

        let git = Git::new();
        let _ = git.run_in(&repo, &["init", "-q"]).unwrap();
        let _ = git
            .run_in(&repo, &["config", "user.email", "test@test"])
            .unwrap();
        let _ = git.run_in(&repo, &["config", "user.name", "test"]).unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "initial"])
            .unwrap();

        // 3 branches:
        // b1: clean verdict at head
        // b2: stale verdict (head moved)
        // b3: unresolved (not in repo)
        let _ = git.run_in(&repo, &["checkout", "-qb", "b1"]).unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "b1 commit"])
            .unwrap();
        let sha_b1 =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();

        let _ = git.run_in(&repo, &["checkout", "-qb", "b2"]).unwrap();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "b2 commit 1"])
            .unwrap();
        let sha_b2_old =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();
        let _ = git
            .run_in(&repo, &["commit", "--allow-empty", "-qm", "b2 commit 2"])
            .unwrap();
        let sha_b2_head =
            String::from_utf8_lossy(&git.run_in(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
                .trim()
                .to_string();

        let db_path = tmp.join("quaqua.db");
        let mut journal = SqliteJournal::open(&db_path).expect("open journal");

        journal
            .record_verdict(
                "wf-1",
                &VerdictRecord {
                    branch: "b1".to_string(),
                    sha: sha_b1.clone(),
                    level: VerdictLevel::ReviewClean,
                    evidence: "clean".to_string(),
                    verifier: "suhuynh-rev".to_string(),
                    source: VerdictSource::Verifier,
                    builder_kind: Some("sude".to_string()),
                    verifier_kind: Some("suhuynh".to_string()),
                    harness: "pi".to_string(),
                },
            )
            .unwrap();

        journal
            .record_verdict(
                "wf-2",
                &VerdictRecord {
                    branch: "b2".to_string(),
                    sha: sha_b2_old.clone(),
                    level: VerdictLevel::UnitTestVerified,
                    evidence: "unit verified".to_string(),
                    verifier: "suhuynh-rev".to_string(),
                    source: VerdictSource::Verifier,
                    builder_kind: Some("sude".to_string()),
                    verifier_kind: Some("suhuynh".to_string()),
                    harness: "pi".to_string(),
                },
            )
            .unwrap();

        journal
            .record_verdict(
                "wf-3",
                &VerdictRecord {
                    branch: "b3".to_string(),
                    sha: "000000000000".to_string(),
                    level: VerdictLevel::UnitTestVerified,
                    evidence: "integ verified".to_string(),
                    verifier: "suhuynh-rev".to_string(),
                    source: VerdictSource::Verifier,
                    builder_kind: Some("sude".to_string()),
                    verifier_kind: Some("suhuynh".to_string()),
                    harness: "pi".to_string(),
                },
            )
            .unwrap();

        let rows = query_branch_verdicts(&db_path, &repo, &git).expect("query verdicts");
        assert_eq!(rows.len(), 3);

        let r1 = rows.iter().find(|r| r.branch == "b1").unwrap();
        assert_eq!(r1.head_sha, sha_b1);
        assert!(!r1.stale);
        assert!(!r1.unresolved);

        let r2 = rows.iter().find(|r| r.branch == "b2").unwrap();
        assert_eq!(r2.head_sha, sha_b2_head);
        assert_eq!(r2.verdict_sha, sha_b2_old);
        assert!(r2.stale);
        assert!(!r2.unresolved);

        let r3 = rows.iter().find(|r| r.branch == "b3").unwrap();
        assert!(r3.head_sha.is_empty());
        assert!(!r3.stale);
        assert!(r3.unresolved);
    }
    struct EnvGuard {
        vars: Vec<(String, Option<String>)>,
    }
    impl EnvGuard {
        fn capture(keys: &[&str]) -> Self {
            let vars = keys
                .iter()
                .map(|k| (k.to_string(), std::env::var(k).ok()))
                .collect();
            Self { vars }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.vars {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod metrics {
    use super::*;
    use crate::engine::journal::Journal;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn test_status_metrics_rendering_and_json_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("status-metrics-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let runs_dir = tmp.join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();

        let tele_path = runs_dir.join("telemetry.jsonl");

        // Old legacy row without rec
        let old_pipeline_row = r#"{"ts":"2026-10-21T10:00:00+07:00","task":"w2","repo":"/Personal/baroiplayer","builder":"sude","reviewer":"quansu","review":"run","rounds":2,"review_changed_outcome":false,"status":"clean","minutes":84}"#;

        // New pipeline row with rec: "pipeline"
        let new_pipeline_row = r#"{"ts":"2026-10-21T11:00:00+07:00","rec":"pipeline","task":"w6","repo":"/Personal/baroiplayer","builder":"sude","reviewer":"quansu","review":"run","rounds":1,"review_changed_outcome":true,"status":"blocked","minutes":27}"#;

        // Worker rows
        let worker_row1 = r#"{"ts":"2026-10-21T12:00:00+07:00","rec":"worker","name":"tieude-p74lock","kind":"tieude","harness":"opencode","owner":"cos","pipeline":null,"repo":"~/quaqua","prompts":1,"exit":0,"wall_sec":409,"budget_sec":1500,"usd":0.0458,"usd_budget":1.0,"tokens":{"in":94574,"out":16991,"reasoning":0,"cache_read":3754496,"cache_write":0},"spend_src":"opencode-db","model":"deepseek-v4.1-flash"}"#;

        let worker_row2 = r#"{"ts":"2026-10-21T12:10:00+07:00","rec":"worker","name":"detu-claudediff","kind":"detu","harness":"omp","owner":"cos","pipeline":null,"repo":"~/Personal/baroiplayer","prompts":1,"exit":0,"wall_sec":420,"budget_sec":2400,"usd":0.41,"usd_budget":2.0,"tokens":{"in":17371,"out":952,"reasoning":0,"cache_read":0,"cache_write":0},"spend_src":"omp-session","model":"gemini-3.8-flash"}"#;

        let worker_row3 = r#"{"ts":"2026-10-21T12:20:00+07:00","rec":"worker","name":"truonglao-metrics","kind":"truonglao","harness":"claude","owner":"cos","pipeline":null,"repo":"~/Personal/baroiplayer","prompts":1,"exit":0,"wall_sec":2280,"budget_sec":2400,"usd":null,"usd_budget":3.0,"tokens":{"in":6995,"out":1262,"reasoning":0,"cache_read":1262078,"cache_write":0},"spend_src":"claude-tokens","model":"claude-opus-5-5"}"#;

        std::fs::write(
            &tele_path,
            format!("{old_pipeline_row}\n{new_pipeline_row}\n{worker_row1}\n{worker_row2}\n{worker_row3}\n"),
        ).unwrap();

        // Test text rendering
        let args = MetricsArgs {
            metrics: true,
            since: "2026-10-21T00:00:00Z".to_string(),
            owner: None,
            json: false,
        };

        let rendered = render_metrics(&runs_dir, &args).unwrap();
        assert!(
            rendered.contains("=== Metrics (runs/telemetry.jsonl, since 2026-10-21T00:00:00Z) ===")
        );
        assert!(rendered.contains("tieude-p74lock"));
        assert!(rendered.contains("detu-claudediff"));
        assert!(rendered.contains("truonglao-metrics"));
        assert!(rendered.contains("0.046/1.00"));
        assert!(rendered.contains("0.41/2.00"));
        assert!(rendered.contains("-/3.00"));
        assert!(rendered.contains("95k/17k/3.8M"));
        assert!(rendered.contains("pipelines: w2 clean r2 84m · w6 blocked r1 27m"));
        assert!(rendered.contains("total: 3 workers · $0.46 (+1 claude unpriced)"));

        // Test --json round-trip
        let json_args = MetricsArgs {
            metrics: true,
            since: "2026-10-21T00:00:00Z".to_string(),
            owner: None,
            json: true,
        };
        let json_str = render_metrics(&runs_dir, &json_args).unwrap();
        let parsed_report: MetricsReport = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed_report.workers.len(), 3);
        assert_eq!(parsed_report.pipelines.len(), 2);
        assert_eq!(parsed_report.total.workers, 3);
        assert!((parsed_report.total.usd - 0.4558).abs() < 1e-3);
        assert_eq!(parsed_report.total.claude_unpriced, 1);

        // Test --owner filter
        let cos_args = MetricsArgs {
            metrics: true,
            since: "2026-10-21T00:00:00Z".to_string(),
            owner: Some("cos".to_string()),
            json: false,
        };
        let cos_rendered = render_metrics(&runs_dir, &cos_args).unwrap();
        assert!(cos_rendered.contains("tieude-p74lock"));

        let other_args = MetricsArgs {
            metrics: true,
            since: "2026-10-21T00:00:00Z".to_string(),
            owner: Some("other-owner".to_string()),
            json: false,
        };
        let other_rendered = render_metrics(&runs_dir, &other_args).unwrap();
        assert!(!other_rendered.contains("tieude-p74lock"));
        assert!(other_rendered.contains("total: 0 workers"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_worker_telemetry_row_roundtrip_unpriced_and_pipeline_cap() {
        let tmp = std::env::temp_dir().join(format!("status-metrics-ext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let runs_dir = tmp.join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        let tele_path = runs_dir.join("telemetry.jsonl");

        let now_sec = crate::util::now_sec();
        let now_ms = now_sec * 1000;

        // 1. Round-trip WorkerTelemetryRow with repo ~/quaqua and unpriced non-claude worker
        let row = WorkerTelemetryRow {
            ts: crate::usage::format_iso_vn(now_ms as i64),
            rec: "worker".to_string(),
            name: "sude-rt".to_string(),
            kind: Some("sude".to_string()),
            harness: Some("omp".to_string()),
            owner: Some("cos".to_string()),
            pipeline: None,
            repo: Some("~/quaqua".to_string()),
            prompts: Some(1),
            exit: Some(0),
            wall_sec: Some(300),
            budget_sec: Some(1500),
            usd: None, // OMP reader failed
            usd_budget: Some(1.0),
            tokens: None,
            spend_src: "omp-session-not-found".to_string(),
            model: None,
        };
        let row_json = serde_json::to_string(&row).unwrap();

        // 2. Ten pipeline items to test newest 8 cap and +N more
        let mut lines = vec![row_json];
        for i in 1..=10 {
            let p = PipelineTelemetryItem {
                ts: crate::usage::format_iso_vn((now_ms + (i as u64) * 1000) as i64),
                rec: "pipeline".to_string(),
                task: format!("task-{i}"),
                repo: "~/quaqua".to_string(),
                builder: "sude".to_string(),
                reviewer: "suhuynh".to_string(),
                review: "run".to_string(),
                rounds: 1,
                review_changed_outcome: false,
                status: "clean".to_string(),
                minutes: 10 + i as u64,
            };
            lines.push(serde_json::to_string(&p).unwrap());
        }
        std::fs::write(&tele_path, lines.join("\n") + "\n").unwrap();

        let args = MetricsArgs {
            metrics: true,
            since: "24h".to_string(),
            owner: None,
            json: false,
        };

        let rendered = render_metrics(&runs_dir, &args).unwrap();
        // Round-trip repo was read properly
        let report = read_metrics(&runs_dir, &args);
        assert_eq!(report.workers.len(), 1);
        assert_eq!(report.workers[0].repo.as_deref(), Some("~/quaqua"));
        // Unpriced marker is general (+1 unpriced), not (+1 claude unpriced)
        assert_eq!(report.total.unpriced, 1);
        assert_eq!(report.total.claude_unpriced, 0);
        assert!(rendered.contains("total: 1 workers · $0.00 (+1 unpriced)"));

        // Pipelines capped at newest 8 (task-3 to task-10), task-1 and task-2 omitted
        assert!(rendered.contains("+2 more"));
        assert!(rendered.contains("task-10 clean r1 20m"));
        assert!(rendered.contains("task-3 clean r1 13m"));
        assert!(!rendered.contains("task-1 clean"));
        assert!(!rendered.contains("task-2 clean"));

        // 3. Test --since cutoff
        let c_24h = parse_since_cutoff("24h", now_ms).unwrap();
        let c_7d = parse_since_cutoff("7d", now_ms).unwrap();
        assert!(c_24h > c_7d);

        // Nit 10: large days value saturates without overflow
        let c_overflow = parse_since_cutoff("999999999999d", now_ms).unwrap();
        assert_eq!(c_overflow, 0);

        // Nit 11: invalid --since value is rejected with Err
        assert!(parse_metrics_args(&[
            "--metrics".to_string(),
            "--since".to_string(),
            "1w".to_string()
        ])
        .is_err());
        assert!(
            parse_metrics_args(&["--metrics".to_string(), "--since=invalid".to_string()]).is_err()
        );

        // Nit 14: long names expand name_width
        let long_worker = WorkerTelemetryRow {
            ts: crate::usage::format_iso_vn(now_ms as i64),
            rec: "worker".to_string(),
            name: "suhuynh-metricsrev".to_string(),
            kind: Some("suhuynh".to_string()),
            harness: Some("omp".to_string()),
            owner: Some("cos".to_string()),
            pipeline: None,
            repo: Some("~/quaqua".to_string()),
            prompts: Some(1),
            exit: Some(0),
            wall_sec: Some(300),
            budget_sec: Some(1500),
            usd: Some(1.23),
            usd_budget: Some(2.0),
            tokens: None,
            spend_src: "omp-session".to_string(),
            model: None,
        };
        let long_report = MetricsReport {
            since: "24h".to_string(),
            workers: vec![long_worker],
            pipelines: vec![],
            total: MetricsTotal {
                workers: 1,
                usd: 1.23,
                unpriced: 0,
                claude_unpriced: 0,
                wall_sec: 300,
            },
            journal: JournalMetrics::default(),
            journal_error: None,
        };
        let rendered_long = render_metrics_text(&long_report);
        // 18 chars for suhuynh-metricsrev means header "name" is padded to 18 chars
        assert!(rendered_long.contains("name               kind/harness"));
        assert!(rendered_long.contains("suhuynh-metricsrev suhuynh/omp"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn temp_runs_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("status-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runs_dir = dir.join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        dir
    }

    fn append_event(db: &Path, wf_id: &str, ts_ms: u64, kind: &str, payload: &str) {
        let mut journal = SqliteJournal::open(db).expect("open journal");
        journal
            .append_event(
                wf_id,
                &crate::engine::journal::EventRecord::new(0, ts_ms, kind, payload),
            )
            .expect("append event");
    }

    fn verdict_payload(level: VerdictLevel, sha: &str) -> String {
        serde_json::to_string(&VerdictRecord {
            branch: "quaqua/s7".to_string(),
            sha: sha.to_string(),
            level,
            evidence: "cargo test --locked -p quaqua metrics".to_string(),
            verifier: "suhuynh-s7rev".to_string(),
            source: VerdictSource::Verifier,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("suhuynh".to_string()),
            harness: "omp".to_string(),
        })
        .unwrap()
    }

    fn gap_payload(name: &str, missing: &[u32], unreasoned: &[u32]) -> String {
        serde_json::json!({
            "name": name,
            "missing": missing,
            "unreasoned": unreasoned,
        })
        .to_string()
    }

    fn metrics_args() -> MetricsArgs {
        MetricsArgs {
            metrics: true,
            since: "24h".to_string(),
            owner: None,
            json: false,
        }
    }

    #[test]
    fn test_metrics_verdicts_by_level_and_playbook_gaps() {
        let tmp = temp_runs_dir("metrics-journal");
        let runs_dir = tmp.join("runs");
        let db = runs_dir.join("quaqua.db");
        let now_ms = crate::util::now_sec() * 1000;

        append_event(
            &db,
            "worker:sude-s7#1",
            now_ms,
            "verdict",
            &verdict_payload(VerdictLevel::UnitTestVerified, "aaaa1111"),
        );
        append_event(
            &db,
            "worker:sude-s7b#1",
            now_ms,
            "verdict",
            &verdict_payload(VerdictLevel::UnitTestVerified, "bbbb2222"),
        );
        append_event(
            &db,
            "worker:suhuynh-s7r#1",
            now_ms,
            "verdict",
            &verdict_payload(VerdictLevel::ReviewClean, "cccc3333"),
        );
        append_event(
            &db,
            "worker:suhuynh-s7r#1",
            now_ms,
            "verdict",
            &verdict_payload(VerdictLevel::Inconclusive, "dddd4444"),
        );
        // Outside the 24h window: still a verdict for G1, not counted by level.
        append_event(
            &db,
            "worker:sude-old#1",
            1_000,
            "verdict",
            &verdict_payload(VerdictLevel::VerifierFailed, "eeee5555"),
        );

        append_event(
            &db,
            "worker:sude-s7#1",
            now_ms,
            "playbook_gap",
            &gap_payload("feature", &[1, 2], &[3]),
        );
        append_event(
            &db,
            "worker:sude-s7b#1",
            now_ms,
            "playbook_gap",
            &gap_payload("feature", &[1], &[]),
        );
        append_event(
            &db,
            "worker:sude-s7c#1",
            now_ms,
            "playbook_gap",
            &gap_payload("bug-fix", &[4, 5], &[6]),
        );

        let report = read_metrics(&runs_dir, &metrics_args());
        assert_eq!(report.journal_error, None);
        assert_eq!(report.journal.verdicts_total, 4);
        assert_eq!(report.journal.verdicts.get("unit-test-verified"), Some(&2));
        assert_eq!(report.journal.verdicts.get("review-clean"), Some(&1));
        assert_eq!(report.journal.verdicts.get("inconclusive"), Some(&1));
        assert_eq!(report.journal.verdicts.get("verifier-failed"), None);

        assert_eq!(report.journal.playbook_gap_events, 3);
        let feature = report.journal.playbook_gaps.get("feature").unwrap();
        assert_eq!(
            (feature.events, feature.missing, feature.unreasoned),
            (2, 3, 1)
        );
        let bug = report.journal.playbook_gaps.get("bug-fix").unwrap();
        assert_eq!((bug.events, bug.missing, bug.unreasoned), (1, 2, 1));
        assert_eq!(report.journal.playbook_gap_missing, 5);
        assert_eq!(report.journal.playbook_gap_unreasoned, 2);

        let rendered = render_metrics(&runs_dir, &metrics_args()).unwrap();
        assert!(rendered
            .contains("verdicts: 4 total - inconclusive=1 review-clean=1 unit-test-verified=2\n"));
        assert!(rendered.contains(
            "playbook_gap: 3 events - bug-fix=1 (missing 2, unreasoned 1) - feature=2 (missing 3, unreasoned 1) - total missing 5 unreasoned 2\n"
        ));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_metrics_exit6_resumes() {
        let tmp = temp_runs_dir("metrics-exits");
        let runs_dir = tmp.join("runs");
        let db = runs_dir.join("quaqua.db");
        let now_ms = crate::util::now_sec() * 1000;

        append_event(&db, "worker:aa#1", now_ms, "workflow_started", "{}");
        append_event(
            &db,
            "worker:aa#1",
            now_ms,
            "workflow_completed",
            r#"{"exit":0}"#,
        );

        // bb hit exit 6 and was prompted again as #2: a resume.
        append_event(
            &db,
            "worker:bb#1",
            now_ms,
            "workflow_completed",
            r#"{"exit":6}"#,
        );
        append_event(&db, "worker:bb#2", now_ms, "workflow_started", "{}");

        // cc hit exit 6 and was never resumed.
        append_event(
            &db,
            "worker:cc#1",
            now_ms,
            "workflow_completed",
            r#"{"exit":6}"#,
        );

        // Other exits are neither accepted nor exit-6.
        append_event(
            &db,
            "worker:dd#1",
            now_ms,
            "workflow_completed",
            r#"{"exit":3}"#,
        );

        let report = read_metrics(&runs_dir, &metrics_args());
        assert_eq!(report.journal.accepted_reports, 1);
        assert_eq!(report.journal.exit6, 2);
        assert_eq!(report.journal.exit6_resumed, 1);

        let rendered = render_metrics(&runs_dir, &metrics_args()).unwrap();
        assert!(
            rendered.contains("exits: 1 accepted, 2 exit-6, 1 resumed after exit 6 (G2 100.0%)\n")
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn verdict_record(sha: &str, source: VerdictSource, level: VerdictLevel) -> VerdictRecord {
        VerdictRecord {
            branch: "quaqua/s7".to_string(),
            sha: sha.to_string(),
            level,
            evidence: "cargo test".to_string(),
            verifier: "suhuynh-s7rev".to_string(),
            source,
            builder_kind: Some("sude".to_string()),
            verifier_kind: Some("suhuynh".to_string()),
            harness: "omp".to_string(),
        }
    }

    fn merge_commit(repo: &str, target: &str, sha: &str, incoming: &[&str]) -> MergeCommit {
        MergeCommit {
            repo: PathBuf::from(repo),
            target: target.to_string(),
            sha: sha.to_string(),
            incoming: incoming.iter().map(|h| h.to_string()).collect(),
            target_truncated: false,
        }
    }

    #[test]
    fn test_metrics_merge_without_pass_requires_verifier_pass() {
        let commits = MergeScan::from_commits(vec![merge_commit(
            "/repo/a",
            "origin/main",
            "m1",
            &[
                "aaaa111122223333", // verifier pass: covered
                "bbbb222233334444", // self-report only: not covered (F6)
                "cccc333344445555", // verifier failed
                "dddd444455556666", // verifier blocked
                "eeee555566667777", // verifier inconclusive
                "ffff666677778888", // no journal record at all
            ],
        )]);
        let verdicts = vec![
            verdict_record(
                "aaaa111122223333",
                VerdictSource::Verifier,
                VerdictLevel::UnitTestVerified,
            ),
            verdict_record(
                "bbbb222233334444",
                VerdictSource::SelfReport,
                VerdictLevel::UnitTestVerified,
            ),
            verdict_record(
                "cccc333344445555",
                VerdictSource::Verifier,
                VerdictLevel::VerifierFailed,
            ),
            verdict_record(
                "dddd444455556666",
                VerdictSource::Verifier,
                VerdictLevel::VerifierBlocked,
            ),
            verdict_record(
                "eeee555566667777",
                VerdictSource::Verifier,
                VerdictLevel::Inconclusive,
            ),
        ];

        let tally = count_merges_without_pass(&commits, &verdicts);
        assert_eq!(tally.merges, 1);
        assert_eq!(tally.checked, 6);
        assert_eq!(tally.without_verdict, 5);
        assert_eq!(tally.no_verdict, 1);
        assert_eq!(tally.not_pass, 4);
        assert_eq!(tally.samples.first().unwrap(), "origin/main@bbbb222");
        assert_eq!(
            tally.repos,
            vec![MergeRepoStat {
                repo: "/repo/a".to_string(),
                refs: vec![MergeRefStat {
                    target: "origin/main".to_string(),
                    merges: 1,
                    truncated: false,
                }],
                merges: 1,
                checked: 6,
                without_verdict: 5,
            }]
        );

        // No verdict records at all: every incoming head is unverified.
        let tally = count_merges_without_pass(&commits, &[]);
        assert_eq!(
            (
                tally.checked,
                tally.without_verdict,
                tally.no_verdict,
                tally.not_pass
            ),
            (6, 6, 6, 0)
        );
    }

    fn fake_output(ok: bool, stdout: &str) -> std::process::Output {
        std::process::Output {
            status: ExitStatusExt::from_raw(if ok { 0 } else { 1 }),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    struct MergeGit {
        heads: Vec<String>,
        origin_main: bool,
        logs: std::collections::BTreeMap<String, String>,
        calls: std::cell::RefCell<Vec<Vec<String>>>,
    }

    impl MergeGit {
        fn new(heads: &[&str], origin_main: bool) -> Self {
            Self {
                heads: heads.iter().map(|h| h.to_string()).collect(),
                origin_main,
                logs: std::collections::BTreeMap::new(),
                calls: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn with_log(self, target: &str, lines: &[&str]) -> Self {
            let mut s = self;
            s.logs.insert(target.to_string(), lines.join("\n"));
            s
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl GitOps for MergeGit {
        fn run_in(&self, _dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|a| a.to_string()).collect());
            if args.contains(&"for-each-ref") {
                Ok(fake_output(true, &self.heads.join("\n")))
            } else if args.contains(&"rev-parse") {
                Ok(fake_output(self.origin_main, ""))
            } else if args.contains(&"log") {
                let target = args.last().copied().unwrap_or("");
                let out = self.logs.get(target).cloned().unwrap_or_default();
                Ok(fake_output(true, &out))
            } else {
                Ok(fake_output(true, ""))
            }
        }

        fn run(&self, _args: &[&str]) -> std::io::Result<std::process::Output> {
            Ok(fake_output(true, ""))
        }

        fn rev_parse_commit(&self, _repo: &Path, _base: &str) -> bool {
            self.origin_main
        }

        fn worktree_list_porcelain(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
            Ok(Vec::new())
        }

        fn worktree_add(
            &self,
            _repo: &Path,
            _branch: &str,
            _wt: &Path,
            _base: Option<&str>,
        ) -> std::io::Result<std::process::Output> {
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
        ) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }

        fn worktree_remove(
            &self,
            _repo: &Path,
            _wt: &Path,
        ) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }

        fn remote_list(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
            Ok(Vec::new())
        }

        fn is_core_bare(&self, _repo: &Path) -> bool {
            false
        }

        fn has_core_worktree(&self, _repo: &Path) -> bool {
            false
        }

        fn enable_worktree_config(&self, _repo: &Path) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }

        fn set_worktree_pushurl_blocked(&self, _wt: &Path, _remote: &str) -> std::io::Result<()> {
            Ok(())
        }

        fn is_git_dir(&self, _cwd: &Path) -> bool {
            true
        }

        fn diff_and_status(&self, _cwd: &Path) -> (String, String) {
            (String::new(), "clean".to_string())
        }
    }

    #[test]
    fn test_metrics_merge_targets_prefer_upstream_and_cover_integ_branches() {
        let repo = Path::new("/repo/a");
        let heads = [
            "main",
            "quaqua/pstack-int2",
            "quaqua/pstack-int",
            "quaqua/s7-integ",
            "quaqua/s7",
            "develop",
        ];
        assert_eq!(
            merge_targets(repo, &MergeGit::new(&heads, true)),
            vec![
                "origin/main",
                "quaqua/pstack-int",
                "quaqua/pstack-int2",
                "quaqua/s7-integ"
            ]
        );
        // No upstream ref: fall back to the local branch.
        assert_eq!(
            merge_targets(repo, &MergeGit::new(&heads, false)),
            vec![
                "main",
                "quaqua/pstack-int",
                "quaqua/pstack-int2",
                "quaqua/s7-integ"
            ]
        );

        assert!(is_integration_branch("quaqua/pstack-int2"));
        assert!(is_integration_branch("quaqua/s7-integ"));
        assert!(!is_integration_branch("quaqua/s7"));
        assert!(!is_integration_branch("main"));
        assert!(!is_integration_branch("quaqua/integration"));
    }

    #[test]
    fn test_metrics_collect_merge_commits_window_parents_and_dedup() {
        let repo = PathBuf::from("/repo/a");
        let git = MergeGit::new(&["main", "quaqua/pstack-int2"], true)
            .with_log(
                "origin/main",
                &[
                    "m1 aaa bbb",
                    "m1 aaa bbb", // same merge reached twice: counted once
                    "m2 ccc",     // one parent: not a merge
                    "m3 ddd eee fff",
                ],
            )
            .with_log("quaqua/pstack-int2", &["m3 ddd eee fff"]);

        let commits = collect_merge_commits(std::slice::from_ref(&repo), &git, 1_700_000_000_000);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].repo, repo);
        assert_eq!(commits[0].target, "origin/main");
        assert_eq!(commits[0].sha, "m1");
        // Incoming heads are every parent after the first.
        assert_eq!(commits[0].incoming, vec!["bbb".to_string()]);
        assert_eq!(commits[1].sha, "m3");
        assert_eq!(
            commits[1].incoming,
            vec!["eee".to_string(), "fff".to_string()]
        );

        // F1: the window reaches git as a committer-date cutoff (seconds).
        let log_calls: Vec<Vec<String>> = git
            .calls()
            .into_iter()
            .filter(|c| c.iter().any(|a| a == "log"))
            .collect();
        assert!(!log_calls.is_empty());
        for call in &log_calls {
            assert!(call.iter().any(|a| a == "--since=@1700000000"));
            assert!(call.iter().any(|a| a == "-n"));
            assert!(call.iter().any(|a| a == "501"));
        }

        // Cutoff 0 means all history: no `--since` argument at all.
        let git = MergeGit::new(&["main"], true).with_log("origin/main", &["m1 aaa bbb"]);
        let _ = collect_merge_commits(&[repo], &git, 0);
        assert!(git
            .calls()
            .iter()
            .filter(|c| c.iter().any(|a| a == "log"))
            .all(|c| !c.iter().any(|a| a.starts_with("--since="))));
    }

    #[test]
    fn test_metrics_g1_wiring_reads_git_and_counts_without_events_table() {
        let tmp = temp_runs_dir("metrics-g1wiring");
        let runs_dir = tmp.join("runs");
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = MergeGit::new(&["main", "quaqua/pstack-int2"], true)
            .with_log("origin/main", &["m1 aaaa1111 bbbb2222"])
            .with_log("quaqua/pstack-int2", &["m2 cccc3333 dddd4444"]);

        let db = runs_dir.join("quaqua.db");
        let now_ms = crate::util::now_sec() * 1000;
        append_event(
            &db,
            "worker:suke#1",
            now_ms,
            "verdict",
            &verdict_payload(VerdictLevel::UnitTestVerified, "bbbb2222"),
        );
        let metrics = load_journal_metrics(&db, 0, std::slice::from_ref(&repo), &git).unwrap();
        assert_eq!(metrics.merges, 2);
        assert_eq!(metrics.merges_checked, 2);
        assert_eq!(metrics.merges_without_verdict, 1);
        assert_eq!(metrics.merges_no_verdict, 1);
        assert_eq!(metrics.merges_not_pass, 0);
        assert_eq!(metrics.merge_repos, 1);
        assert_eq!(
            metrics.merges_per_repo,
            vec![MergeRepoStat {
                // The report prints a repo under `$HOME` as `~/...`; that is a
                // rendering, so expect the display form of the scanned repo.
                repo: display_repo(&repo),
                refs: vec![
                    MergeRefStat {
                        target: "origin/main".to_string(),
                        merges: 1,
                        truncated: false,
                    },
                    MergeRefStat {
                        target: "quaqua/pstack-int2".to_string(),
                        merges: 1,
                        truncated: false,
                    },
                ],
                merges: 2,
                checked: 2,
                without_verdict: 1,
            }]
        );

        let report = MetricsReport {
            since: "7d".to_string(),
            workers: Vec::new(),
            pipelines: Vec::new(),
            total: MetricsTotal {
                workers: 0,
                usd: 0.0,
                unpriced: 0,
                claude_unpriced: 0,
                wall_sec: 0,
            },
            journal: metrics,
            journal_error: None,
        };
        let text = render_metrics_text(&report);
        assert!(text.contains(
            "merges: 2 merge(s) (2 incoming heads), 1 without a verifier pass at the head (G1) - 1 with no verdict, 0 failed/blocked/inconclusive - 1 repo(s), from git (no merge event is journaled), since 7d - quaqua/pstack-int2@dddd444\n"
        ));
        assert!(text.contains(&format!(
            "  merges {}: 2 merge(s), 2 head(s), 1 without - refs origin/main=1, quaqua/pstack-int2=1\n",
            display_repo(&repo)
        )));

        // A journal without an events table cannot zero G1: git still counts.
        let empty = runs_dir.join("empty.db");
        rusqlite::Connection::open(&empty)
            .unwrap()
            .execute_batch("CREATE TABLE other (x INTEGER);")
            .unwrap();
        let metrics = load_journal_metrics(&empty, 0, &[repo], &git).unwrap();
        assert_eq!(metrics.merges_checked, 2);
        assert_eq!(metrics.merges_without_verdict, 2);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_metrics_journal_unavailable_prints_a_line() {
        let tmp = temp_runs_dir("metrics-nodb");
        let runs_dir = tmp.join("runs");

        let rendered = render_metrics(&runs_dir, &metrics_args()).unwrap();
        assert!(rendered.contains("journal: unavailable - journal not found at"));
        assert!(rendered.starts_with("=== Metrics (runs/telemetry.jsonl, since 24h) ==="));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_metrics_cap_truncation_notice_in_text_and_json() {
        let tmp = temp_runs_dir("metrics-truncation");
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        // 501 merges: exceeds MERGE_SCAN_LIMIT (500) by 1
        let mut log_501 = Vec::new();
        for i in 0..501 {
            log_501.push(format!("m{i:04} p{i:04}_1 p{i:04}_2"));
        }
        let log_501_refs: Vec<&str> = log_501.iter().map(|s| s.as_str()).collect();

        let git = MergeGit::new(&["main"], true).with_log("origin/main", &log_501_refs);
        let commits = collect_merge_commits(std::slice::from_ref(&repo), &git, 0);
        assert_eq!(commits.len(), 500);
        assert!(commits.iter().all(|c| c.target_truncated));

        let tally = count_merges_without_pass(&commits, &[]);
        assert_eq!(tally.repos.len(), 1);
        let ref_stat = &tally.repos[0].refs[0];
        assert_eq!(ref_stat.target, "origin/main");
        assert_eq!(ref_stat.merges, 500);
        assert!(ref_stat.truncated);

        let report = MetricsReport {
            since: "7d".to_string(),
            workers: Vec::new(),
            pipelines: Vec::new(),
            total: MetricsTotal::default(),
            journal: JournalMetrics {
                merges: tally.merges,
                merges_checked: tally.checked,
                merges_without_verdict: tally.without_verdict,
                merges_no_verdict: tally.no_verdict,
                merges_not_pass: tally.not_pass,
                merges_missing_samples: tally.samples,
                merge_repos: 1,
                merges_per_repo: tally.repos,
                truncated: tally.truncated,
                ..Default::default()
            },
            journal_error: None,
        };

        // Text output surfaces the truncation notice on that ref's line
        let text = render_metrics_text(&report);
        assert!(text.contains("refs origin/main=500 (capped)"));
        assert!(text.contains("merges: 500 merge(s) (capped)"));
        let json_str = serde_json::to_string_pretty(&report).unwrap();
        assert!(json_str.contains("\"truncated\": true"));
        let json_val: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(json_val["journal"]["truncated"], true);

        let deserialized: MetricsReport = serde_json::from_str(&json_str).unwrap();
        assert!(deserialized.journal.merges_per_repo[0].refs[0].truncated);
        assert!(deserialized.journal.truncated);

        // When exactly at or below cap (e.g. 500 merges): truncated must be false
        let log_500_refs = &log_501_refs[..500];
        let git_500 = MergeGit::new(&["main"], true).with_log("origin/main", log_500_refs);
        let commits_500 = collect_merge_commits(std::slice::from_ref(&repo), &git_500, 0);
        assert_eq!(commits_500.len(), 500);
        assert!(!commits_500.iter().any(|c| c.target_truncated));

        let tally_500 = count_merges_without_pass(&commits_500, &[]);
        let ref_stat_500 = &tally_500.repos[0].refs[0];
        assert_eq!(ref_stat_500.merges, 500);
        assert!(!ref_stat_500.truncated);

        let report_500 = MetricsReport {
            journal: JournalMetrics {
                merges_per_repo: tally_500.repos,
                truncated: tally_500.truncated,
                ..Default::default()
            },
            ..report.clone()
        };
        let text_500 = render_metrics_text(&report_500);
        assert!(text_500.contains("refs origin/main=500\n"));
        assert!(!text_500.contains("(capped)"));

        let json_500 = serde_json::to_string_pretty(&report_500).unwrap();
        assert!(!json_500.contains("\"truncated\""));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_metrics_g1_ref_stays_capped_when_all_commits_deduped() {
        let tmp =
            std::env::temp_dir().join(format!("quaqua-test-metrics-dedup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let projects = tmp.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let repo = tmp.join("repo-dedup");
        std::fs::create_dir_all(&repo).unwrap();

        std::fs::write(
            projects.join("INDEX.md"),
            format!(
                "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| p1 | {} | user | active | g |\n",
                repo.display()
            ),
        )
        .unwrap();
        std::fs::write(
            projects.join("p1.md"),
            "# p1\nIntegration branch: `quaqua/pstack-int2`\n",
        )
        .unwrap();

        let mut log_main = Vec::new();
        for i in 0..500 {
            log_main.push(format!("m{i:04} p{i:04}_1 p{i:04}_2"));
        }
        let log_main_refs: Vec<&str> = log_main.iter().map(|s| s.as_str()).collect();

        let mut log_integ = Vec::new();
        for i in 0..500 {
            log_integ.push(format!("m{i:04} p{i:04}_1 p{i:04}_2"));
        }
        log_integ.push("m0500 extra1 extra2".to_string());
        let log_integ_refs: Vec<&str> = log_integ.iter().map(|s| s.as_str()).collect();

        let git = MergeGit::new(&["main", "quaqua/pstack-int2"], true)
            .with_log("origin/main", &log_main_refs)
            .with_log("quaqua/pstack-int2", &log_integ_refs);

        let commits = collect_merge_commits(std::slice::from_ref(&repo), &git, 0);
        let tally = count_merges_without_pass(&commits, &[]);

        let repo_stat = &tally.repos[0];
        let integ_ref = repo_stat
            .refs
            .iter()
            .find(|r| r.target == "quaqua/pstack-int2")
            .expect("capped ref should remain present even if all kept commits are deduped");
        assert_eq!(integ_ref.merges, 0);
        assert!(integ_ref.truncated);

        let report = MetricsReport {
            since: "7d".to_string(),
            workers: Vec::new(),
            pipelines: Vec::new(),
            total: MetricsTotal::default(),
            journal: JournalMetrics {
                merges: tally.merges,
                merges_checked: tally.checked,
                merges_without_verdict: tally.without_verdict,
                merges_no_verdict: tally.no_verdict,
                merges_not_pass: tally.not_pass,
                merges_missing_samples: tally.samples,
                merge_repos: 1,
                merges_per_repo: tally.repos,
                truncated: tally.truncated,
                ..Default::default()
            },
            journal_error: None,
        };

        let text = render_metrics_text(&report);
        assert!(text.contains("quaqua/pstack-int2=0 (capped)"));
        assert!(text.contains("merges: 500 merge(s) (capped)"));

        let json_str = serde_json::to_string_pretty(&report).unwrap();
        let json_val: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(json_val["journal"]["truncated"], true);

        let deserialized: MetricsReport = serde_json::from_str(&json_str).unwrap();
        assert!(deserialized.journal.truncated);
        assert!(
            deserialized.journal.merges_per_repo[0]
                .refs
                .iter()
                .find(|r| r.target == "quaqua/pstack-int2")
                .unwrap()
                .truncated
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
