use super::apply::{apply_ops, CliHerdrDriver, HerdrDriver};
use super::plan::{is_candidate_worker, plan_with_deferred};
use super::reconcile::filter_ops_for_check;
use super::types::{Op, Snapshot};
use std::collections::{HashMap, HashSet};
use std::fs::{create_dir_all, remove_file, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HERDR_PLUGIN_STATE_DIR") {
        PathBuf::from(dir)
    } else if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        PathBuf::from(xdg).join("herdr/plugins/herdr-quaqua")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/state/herdr/plugins/herdr-quaqua")
    } else {
        PathBuf::from(".herdr-quaqua-state")
    }
}

pub fn is_shadow_mode(state_dir: &Path) -> bool {
    let mut shadow_mode = true;
    if let Ok(v) =
        std::env::var("HERDR_QUAQUA_SHADOW").or_else(|_| std::env::var("HERDR_GRID_SHADOW"))
    {
        let lower = v.trim().to_lowercase();
        if lower == "0" || lower == "false" || lower == "off" {
            shadow_mode = false;
        }
    }
    if shadow_mode {
        let shadow_file = state_dir.join("shadow");
        if shadow_file.is_file() {
            if let Ok(content) = std::fs::read_to_string(&shadow_file) {
                let lower = content.trim().to_lowercase();
                if lower.contains("0") || lower.contains("false") || lower.contains("off") {
                    shadow_mode = false;
                }
            }
        }
    }
    shadow_mode
}

pub use crate::util::is_pid_alive;

/// The hook's guard is the shared grid apply lock: same directory, same
/// reentrancy bookkeeping as `grid::lock` (see there for why the lock exists).
pub use super::lock::ApplyLock as LockGuard;

pub fn acquire_lock(lock_dir: &Path, pid_file: &Path) -> Result<LockGuard, String> {
    super::lock::acquire_paths(lock_dir, pid_file)
}

/// Early exit check on pane.agent_status_changed events that change nothing
pub fn should_early_exit(
    event_name: Option<&str>,
    event_json: Option<&str>,
    last_snap_file: &Path,
) -> bool {
    if event_name != Some("pane.agent_status_changed") {
        return false;
    }
    let ej = match event_json {
        Some(s) if !s.is_empty() => s,
        _ => return false,
    };
    if !last_snap_file.is_file() {
        return false;
    }

    let val: serde_json::Value = match serde_json::from_str(ej) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let pid = match val.get("pane_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return false,
    };
    // Finding 8: if new_status is missing/empty, do not early exit
    let new_status = match val.get("agent_status").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return false,
    };

    let last_snap_text = match std::fs::read_to_string(last_snap_file) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let last_val: serde_json::Value = match serde_json::from_str(&last_snap_text) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let snap = match super::types::parse_snapshot_value(&last_val) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let p = match snap.panes.iter().find(|pane| pane.pane_id == pid) {
        Some(pane) => pane,
        None => return true, // Foreign/untracked pane
    };

    let role = p
        .tokens
        .as_ref()
        .and_then(|t| t.grid_role.as_deref())
        .unwrap_or("");
    if role != "worker" {
        return true; // Not a worker pane
    }

    let old_working = p.agent_status.as_deref() == Some("working");
    let new_working = new_status == "working";

    // If working flag unchanged (idle <-> done <-> unknown), early exit
    old_working == new_working
}

/// Compute signature of snapshot matching hook.sh:240-258
pub fn compute_snapshot_signature(snap: &Snapshot) -> String {
    let mut sigs = Vec::new();
    let tab_labels: HashMap<&str, &str> = snap
        .tabs
        .iter()
        .map(|t| (t.tab_id.as_str(), t.label.as_deref().unwrap_or("")))
        .collect();

    for p in &snap.panes {
        let role = p
            .tokens
            .as_ref()
            .and_then(|t| t.grid_role.as_deref())
            .unwrap_or("");
        // Finding 4: All workers with grid_role == "worker"
        if role == "worker" {
            let tid = p.tab_id.as_deref().unwrap_or("");
            let tlabel = tab_labels.get(tid).copied().unwrap_or("");
            // Finding 4: Normalize scratch tabs starting with grid-scratch- to @scratch
            let tab_repr = if tlabel.starts_with("grid-scratch-") {
                "@scratch"
            } else {
                tid
            };
            let status = if p.agent_status.as_deref() == Some("working") {
                "working"
            } else {
                "idle"
            };
            let owner = p
                .tokens
                .as_ref()
                .and_then(|t| t.grid_owner.as_deref())
                .unwrap_or("");
            let group = p
                .tokens
                .as_ref()
                .and_then(|t| t.grid_group.as_deref())
                .unwrap_or("");
            let ready = p
                .tokens
                .as_ref()
                .and_then(|t| t.grid_ready.as_ref())
                .map(|v| v.to_string())
                .unwrap_or_default();
            sigs.push(format!(
                "{}:{}:{}:{}:{}:{}:{}",
                p.pane_id, tab_repr, status, role, owner, group, ready
            ));
        }
    }
    sigs.sort();
    sigs.join(";")
}

fn log_line(log_file: &Path, msg: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_file) {
        let ts = chrono_or_simple_time();
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

fn chrono_or_simple_time() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

pub fn reconcile_step<D: HerdrDriver>(
    sdir: &Path,
    driver: &mut D,
    deferred_statuses: Option<&[&str]>,
) -> Result<(), String> {
    // Finding 8: An empty or failing snapshot returns Ok(()) without failure
    let mut snap = match driver.snapshot() {
        Ok(s) => {
            if s.panes.is_empty() && s.workspaces.is_empty() {
                return Ok(());
            }
            s
        }
        Err(_) => return Ok(()),
    };

    let shadow_log = sdir.join("shadow.log");
    let last_snap_file = sdir.join("last_snapshot.json");
    let failed_ops_file = sdir.join("failed_ops.json");
    let failed_snap_file = sdir.join("failed_snap.json");

    // Closed pane detection via snapshot diff
    if last_snap_file.is_file() {
        if let Ok(last_text) = std::fs::read_to_string(&last_snap_file) {
            if let Ok(last_val) = serde_json::from_str::<serde_json::Value>(&last_text) {
                if let Ok(last_snap) = super::types::parse_snapshot_value(&last_val) {
                    let old_workers: HashSet<String> = last_snap
                        .panes
                        .iter()
                        .filter(|p| is_candidate_worker(p))
                        .map(|p| p.pane_id.clone())
                        .collect();
                    let cur_workers: HashSet<String> = snap
                        .panes
                        .iter()
                        .filter(|p| is_candidate_worker(p))
                        .map(|p| p.pane_id.clone())
                        .collect();
                    for closed in old_workers.difference(&cur_workers) {
                        log_line(
                            &shadow_log,
                            &format!("closed pane detected via snapshot diff: {closed}"),
                        );
                    }
                }
            }
        }
    }

    // In-flight spawn poll (N3): poll only when grid_ready is null or empty (hook.sh:169-173)
    let is_unready_spawn = |p: &super::types::Pane| {
        let is_worker = p.tokens.as_ref().and_then(|t| t.grid_role.as_deref()) == Some("worker");
        let ready_empty = match p.tokens.as_ref().and_then(|t| t.grid_ready.as_ref()) {
            None => true,
            Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::String(s)) => s.is_empty(),
            _ => false,
        };
        is_worker && ready_empty
    };

    let mut has_unready = snap.panes.iter().any(is_unready_spawn);
    if has_unready {
        for _ in 0..15 {
            sleep(Duration::from_millis(200));
            if let Ok(fresh) = driver.snapshot() {
                has_unready = fresh.panes.iter().any(is_unready_spawn);
                snap = fresh;
                if !has_unready {
                    break;
                }
            }
        }
    }

    // Atomically cache snapshot
    if let Ok(snap_json) = serde_json::to_string(&snap) {
        let tmp = last_snap_file.with_extension("tmp");
        if std::fs::write(&tmp, &snap_json).is_ok() {
            let _ = std::fs::rename(tmp, &last_snap_file);
        }
    }
    let ops = plan_with_deferred(&snap, deferred_statuses)?;
    if ops.is_empty() {
        let _ = remove_file(&failed_ops_file);
        let _ = remove_file(&failed_snap_file);
    }

    let shadow_mode = is_shadow_mode(sdir);
    if shadow_mode {
        let ops_json = serde_json::to_string(&ops).unwrap_or_else(|_| "[]".to_string());
        log_line(&shadow_log, &format!("shadow ops: {ops_json}"));
        println!("{ops_json}");
        return Ok(());
    }

    // Live mode: filter runnable ops using the shared convergence engine from reconcile.rs.
    // Owner/workspace stay unscoped here (the hook has no caller pane and must
    // repair every owner's layout); the planner already arranges one workspace
    // at a time, so no op can cross a workspace boundary.
    let mut non_deferred: Vec<Op> = filter_ops_for_check(&ops, &snap, None, None, None, None)
        .into_iter()
        .cloned()
        .collect();

    if non_deferred.is_empty() {
        return Ok(());
    }
    // Failure backoff: avoid error storms when an op fails on an unchanged layout
    let snap_sig = compute_snapshot_signature(&snap);
    if failed_ops_file.is_file() && failed_snap_file.is_file() {
        let last_snap_sig = std::fs::read_to_string(&failed_snap_file).unwrap_or_default();
        if !snap_sig.is_empty() && last_snap_sig == snap_sig {
            let failed_text = std::fs::read_to_string(&failed_ops_file).unwrap_or_default();
            if let Ok(failed_ops) = serde_json::from_str::<Vec<Op>>(&failed_text) {
                let prev_len = non_deferred.len();
                non_deferred.retain(|op| {
                    !failed_ops
                        .iter()
                        .any(|f| f.pane == op.pane && f.target == op.target)
                });
                if non_deferred.len() < prev_len {
                    log_line(&shadow_log, "backoff: skipping previously failed ops");
                }
            }
        } else {
            let _ = remove_file(&failed_ops_file);
            let _ = remove_file(&failed_snap_file);
        }
    }

    if non_deferred.is_empty() {
        return Ok(());
    }

    let non_deferred_json = serde_json::to_string(&non_deferred).unwrap_or_default();
    log_line(&shadow_log, &format!("apply ops: {non_deferred_json}"));

    match apply_ops(driver, &non_deferred, Some(&snap)) {
        Ok(_) => {
            let _ = remove_file(&failed_ops_file);
            let _ = remove_file(&failed_snap_file);
            // Refresh snapshot cache
            if let Ok(post_snap) = driver.snapshot() {
                if let Ok(post_json) = serde_json::to_string(&post_snap) {
                    let tmp = last_snap_file.with_extension("tmp");
                    if std::fs::write(&tmp, &post_json).is_ok() {
                        let _ = std::fs::rename(tmp, &last_snap_file);
                    }
                }
            }
            Ok(())
        }
        Err(e) => {
            log_line(&shadow_log, &format!("apply error: {e}"));
            let _ = std::fs::write(&failed_ops_file, &non_deferred_json);
            // Finding 4: Record the POST-FAILURE snapshot signature
            let mut post_snap_sig = snap_sig;
            if let Ok(err_snap) = driver.snapshot() {
                if let Ok(err_json) = serde_json::to_string(&err_snap) {
                    let tmp = last_snap_file.with_extension("tmp");
                    if std::fs::write(&tmp, &err_json).is_ok() {
                        let _ = std::fs::rename(tmp, &last_snap_file);
                    }
                }
                post_snap_sig = compute_snapshot_signature(&err_snap);
            }
            let _ = std::fs::write(&failed_snap_file, &post_snap_sig);
            Err(e)
        }
    }
}

pub fn run_hook() -> Result<(), String> {
    let sdir = state_dir();
    create_dir_all(&sdir)
        .map_err(|e| format!("cannot create state dir {}: {e}", sdir.display()))?;

    let event_name = std::env::var("HERDR_PLUGIN_EVENT").ok();
    let event_json = std::env::var("HERDR_PLUGIN_EVENT_JSON").ok();
    let last_snap_file = sdir.join("last_snapshot.json");

    // Finding 4: Early exit on irrelevant status events without acquiring lock
    if should_early_exit(
        event_name.as_deref(),
        event_json.as_deref(),
        &last_snap_file,
    ) {
        return Ok(());
    }

    // Finding 9 remainder: deferred_statuses from the one team.toml resolution
    // every command uses ($QUAQUA_TEAM_TOML, ./team.toml, <workspace>/team.toml,
    // embedded). The herdr plugin runs the hook from an arbitrary cwd, so a
    // cwd-only read silently ignored ~/quaqua/team.toml.
    let def_statuses = crate::config::load_team_config()
        .ok()
        .map(|c| c.grid.deferred_statuses);
    let def_slices: Option<Vec<&str>> = def_statuses
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect());

    let pending_file = sdir.join("pending");
    let pid_str = std::process::id().to_string();
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&pending_file)
    {
        let _ = writeln!(f, "{pid_str}");
    }

    let lock_dir = sdir.join("lock");
    let pid_file = lock_dir.join("pid");
    let guard = acquire_lock(&lock_dir, &pid_file)?;

    let mut driver = CliHerdrDriver::new();

    // Main loop handling pending counter
    loop {
        let _ = remove_file(&pending_file);
        reconcile_step(&sdir, &mut driver, def_slices.as_deref())?;
        if !pending_file.is_file() {
            break;
        }
    }

    // Drop lock
    drop(guard);

    // Finding 8: Unlock-window recheck (hook.sh:365-377)
    if pending_file.is_file() {
        if let Ok(_guard2) = acquire_lock(&lock_dir, &pid_file) {
            while pending_file.is_file() {
                let _ = remove_file(&pending_file);
                reconcile_step(&sdir, &mut driver, def_slices.as_deref())?;
            }
        }
    }

    Ok(())
}
