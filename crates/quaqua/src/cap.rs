//! Worker cap calculation and enforcement (`quaqua cap`).

use crate::config::Config;
use crate::herdr::HerdrOps;
use crate::state::{ExecutionState, WorkerMetadata};
use crate::util::is_pid_alive;
use serde_json::Value;
use std::path::Path;

/// Check if a worker name matches `<kind>-...` for any kind in `cfg.kinds` and does not end with `-lead`.
///
/// The config is passed in so callers resolve it once (and can surface a load
/// failure) instead of every check reading process-global env.
pub fn is_task_worker_name(name: &str, cfg: &Config) -> bool {
    crate::config::kind_of_worker(cfg, name).is_some()
}

fn contains_prefix_digit(w: &str, prefix: &str) -> bool {
    let mut search = w;
    while let Some(idx) = search.find(prefix) {
        let after = &search[idx + prefix.len()..];
        if after
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            return true;
        }
        search = &search[idx + prefix.len()..];
    }
    false
}

/// Check if a worker name matches `*-pr[0-9]*` or `*-<task>r[0-9]*` (when task is empty: `*-r[0-9]*`).
pub fn matches_pipeline_reviewer(w: &str, task: &str) -> bool {
    if contains_prefix_digit(w, "-pr") {
        return true;
    }
    let task_pat = format!("-{}r", task);
    contains_prefix_digit(w, &task_pat)
}

/// True for an owner tag that names a project team's Lead (`<slug>-lead`).
pub fn is_lead_owner(owner: &str) -> bool {
    owner
        .strip_suffix("-lead")
        .map(|slug| !slug.is_empty())
        .unwrap_or(false)
}

/// The most live worker slots `owner` may hold: a project Lead is held to
/// `[roles.lead] max_peers` (default `DEFAULT_LEAD_MAX_PEERS`), every other
/// owner — the CoS — to `[limits] max_workers_per_orchestrator`. A project
/// override may tighten the global cap, so it tightens a Lead with it.
pub fn owner_cap(cfg: &Config, owner: &str) -> i64 {
    let global = cfg.limits.max_workers_per_orchestrator;
    if !is_lead_owner(owner) {
        return global;
    }
    let peers = cfg
        .roles
        .get("lead")
        .and_then(|role| role.max_peers)
        .unwrap_or(crate::config::DEFAULT_LEAD_MAX_PEERS);
    peers.min(global)
}

/// Refuse a spawn that needs `weight` slots more than `owner` has left. This is
/// the one gate every spawn path calls (worker, pipeline, taskforce), so the cap
/// a Lead sees is the cap its Peers were counted against.
pub fn check_owner_cap(
    cfg: &Config,
    owner: &str,
    slots_used: i64,
    weight: i64,
    what: &str,
) -> Result<(), String> {
    let cap = owner_cap(cfg, owner);
    if slots_used + weight <= cap {
        return Ok(());
    }
    let source = if is_lead_owner(owner) {
        "roles.lead.max_peers"
    } else {
        "limits.max_workers_per_orchestrator"
    };
    Err(format!(
        "worker cap exceeded ({slots_used} live worker slots for owner '{owner}'; {what} needs {weight}, {source} is {cap})"
    ))
}
/// Derives the effective taskforce weight according to the canonical precedence order:
/// 1. `[limits.weights.<role>]` in config if explicitly configured
/// 2. Execution state `n` if present
/// 3. Execution state `members` count if present and non-empty
/// 4. Layout role `members` count from the resolved layout
/// 5. Per-kind legacy default (dual-lens: 2, council: 3, else 1)
pub fn taskforce_weight(
    cfg: &Config,
    state: &crate::state::TaskforceExecutionState,
    layout: &crate::layout::Layout,
) -> i64 {
    if let Some(&w) = cfg.limits.weights.get(&state.kind) {
        return w;
    }
    if let Some(n) = state.n {
        return n;
    }
    if !state.members.is_empty() {
        return state.members.len() as i64;
    }
    crate::config::taskforce_weight_from_layout(cfg, &state.kind, layout)
}

/// Calculate the number of worker slots used by `owner` (defaults to "cos").
pub fn count_owner_slots<H: HerdrOps>(
    runs_dir: &Path,
    owner: &str,
    exclude_worker: Option<&str>,
    herdr: &H,
    cfg: &Config,
) -> i64 {
    let mut live_workers = Vec::new();
    if let Ok(agent_list_json) = herdr.agent_list() {
        if let Ok(json) = serde_json::from_str::<Value>(&agent_list_json) {
            if let Some(agents) = json
                .get("result")
                .and_then(|r| r.get("agents"))
                .and_then(|a| a.as_array())
            {
                for agent in agents {
                    let status = agent
                        .get("agent_status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    let name = agent.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    if status != "dead"
                        && !name.is_empty()
                        && is_task_worker_name(name, cfg)
                        && Some(name) != exclude_worker
                    {
                        live_workers.push(name.to_string());
                    }
                }
            }
        }
    }

    let mut o_workers = Vec::new();
    for w in &live_workers {
        if !WorkerMetadata::owner_path(runs_dir, w).is_file() {
            continue;
        }
        let w_owner = WorkerMetadata::load_owner(runs_dir, w);
        if w_owner == owner {
            o_workers.push(w.clone());
        }
    }
    let pipe_weight = cfg.limits.weights.get("pipeline").copied().unwrap_or(2);

    let effective_layout = match crate::config::resolve_project_layout(cfg, None) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "warning: [cap count_owner_slots] cannot resolve layout, falling back to legacy: {e}"
            );
            crate::layout::Layout::from_legacy(cfg)
        }
    };
    let mut total_managed_slots: i64 = 0;
    let mut covered_workers: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(runs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let exec_state = match ExecutionState::load(&path) {
                Some(s) if !s.is_finished() => s,
                _ => continue,
            };

            match exec_state {
                ExecutionState::Pipeline(p) => {
                    if p.owner() != owner {
                        continue;
                    }
                    let p_bname = format!("{}-{}", p.builder, p.task);
                    let p_alive = (!p.pipeline_pid.is_empty() && is_pid_alive(&p.pipeline_pid))
                        || live_workers.iter().any(|a| {
                            a == &p_bname
                                || (!p.task.is_empty() && a.ends_with(&format!("-{}", p.task)))
                                || matches_pipeline_reviewer(a, &p.task)
                        });

                    if p_alive {
                        let mut p_cur_live: i64 = 0;
                        for w in &o_workers {
                            let pipe_file = runs_dir.join(format!("{w}.pipeline"));
                            let w_pipe = std::fs::read_to_string(&pipe_file)
                                .map(|s| s.trim().to_string())
                                .unwrap_or_default();
                            let is_reviewer = matches_pipeline_reviewer(w, &p.task);
                            if (!p.task.is_empty()
                                && (w_pipe == p.task || w.ends_with(&format!("-{}", p.task))))
                                || w == &p_bname
                                || is_reviewer
                            {
                                p_cur_live += 1;
                                covered_workers.insert(w.clone());
                            }
                        }
                        total_managed_slots += p_cur_live.max(pipe_weight);
                    }
                }
                ExecutionState::Taskforce(t) => {
                    if t.owner() != owner {
                        continue;
                    }
                    let weight = taskforce_weight(cfg, &t, &effective_layout);

                    let mut t_alive = false;
                    if !t.pid.is_empty() && is_pid_alive(&t.pid) {
                        t_alive = true;
                    } else if !t.task.is_empty() {
                        for w in &live_workers {
                            if WorkerMetadata::load_taskforce(runs_dir, w).as_deref()
                                == Some(&t.task)
                                || t.members.iter().any(|m| m == w)
                            {
                                t_alive = true;
                                break;
                            }
                        }
                    }

                    if t_alive {
                        let mut t_cur_live: i64 = 0;
                        for w in &o_workers {
                            if !t.task.is_empty()
                                && (WorkerMetadata::load_taskforce(runs_dir, w).as_deref()
                                    == Some(&t.task)
                                    || t.members.iter().any(|m| m == w))
                            {
                                t_cur_live += 1;
                                covered_workers.insert(w.clone());
                            }
                        }
                        total_managed_slots += t_cur_live.max(weight);
                    }
                }
            }
        }
    }
    let standalone_slots = o_workers
        .iter()
        .filter(|w| !covered_workers.contains(*w))
        .count() as i64;
    standalone_slots + total_managed_slots
}
