use super::plan::is_candidate_worker;
use super::types::{Op, Snapshot, Target};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveResult {
    pub pane_id: String,
    pub tab_id: String,
    pub previous_pane_id: Option<String>,
}

pub use crate::herdr::HerdrDriver;
pub type CliHerdrDriver = crate::herdr::Herdr;

pub fn parse_move_response(val: &serde_json::Value, orig_pane: &str) -> Result<MoveResult, String> {
    let mr = val
        .get("result")
        .and_then(|r| r.get("move_result"))
        .or_else(|| val.get("move_result"))
        .unwrap_or(val);

    let changed = mr.get("changed").and_then(|v| v.as_bool()).unwrap_or(false);
    let reason = mr
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if !changed {
        return Err(format!(
            "pane.move changed nothing for {orig_pane} (changed=false, reason={reason})"
        ));
    }

    let pane_obj = mr.get("pane");
    let new_pane = pane_obj
        .and_then(|p| p.get("pane_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing pane.pane_id in move result for {orig_pane}"))?
        .to_string();

    let new_tab = pane_obj
        .and_then(|p| p.get("tab_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let prev_pane = mr
        .get("previous_pane_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(MoveResult {
        pane_id: new_pane,
        tab_id: new_tab,
        previous_pane_id: prev_pane,
    })
}

/// Execute layout ops via a HerdrDriver, verifying fresh snapshots before each move,
/// propagating deferrals, parking panes if in-tab repositioning, and returning
/// the remapped ID dictionary.
pub fn apply_ops<D: HerdrDriver>(
    driver: &mut D,
    ops: &[Op],
    _initial_snapshot: Option<&Snapshot>,
) -> Result<HashMap<String, String>, String> {
    let mut pane_map: HashMap<String, String> = HashMap::new();
    let mut latest_tabs: HashMap<(String, String), String> = HashMap::new();
    let mut scratch_counter = 0;

    let init_snap = match _initial_snapshot {
        Some(s) => s.clone(),
        None => driver.snapshot()?,
    };
    let pane_group = |pid: &str| -> String {
        init_snap
            .panes
            .iter()
            .find(|p| p.pane_id == pid)
            .and_then(|p| p.tokens.as_ref())
            .and_then(|t| t.grid_group.as_deref())
            .unwrap_or("workers")
            .to_string()
    };

    let mut deferred_panes: HashSet<String> = HashSet::new();
    let mut deferred_tabs: HashSet<(String, String)> = HashSet::new();
    let mut group_new_tab_count: HashMap<String, usize> = HashMap::new();
    let mut group_created_new_tab_count: HashMap<String, usize> = HashMap::new();

    for op in ops {
        let grp = pane_group(&op.pane);
        // Finding 3: Propagate deferral to dependent ops
        if op.deferred {
            deferred_panes.insert(op.pane.clone());
            if matches!(op.target, Target::NewTab { .. }) {
                let cnt = group_new_tab_count.entry(grp.clone()).or_insert(0);
                let tab_ref = if *cnt == 0 { "@workers" } else { "@workers-2" };
                deferred_tabs.insert((grp.clone(), tab_ref.to_string()));
                *cnt += 1;
            }
            continue;
        }

        if matches!(op.target, Target::NewTab { .. }) {
            let cnt = group_new_tab_count.entry(grp.clone()).or_insert(0);
            *cnt += 1;
        }

        if let Target::Split { tab, anchor, .. } = &op.target {
            let tab_def = if tab == "@workers" || tab == "@workers-2" {
                deferred_tabs.contains(&(grp.clone(), tab.clone()))
            } else {
                false
            };
            if deferred_panes.contains(anchor) || tab_def {
                deferred_panes.insert(op.pane.clone());
                continue;
            }
        }

        let actual_pane = pane_map
            .get(&op.pane)
            .cloned()
            .unwrap_or_else(|| op.pane.clone());

        // Finding 5: Fetch fresh snapshot before each move to verify pane state
        let fresh_snap = driver.snapshot()?;
        let pane_info = fresh_snap.panes.iter().find(|p| p.pane_id == actual_pane);
        let pane_obj = match pane_info {
            Some(p) => p,
            None => return Err(format!("apply: pane {actual_pane} missing from snapshot")),
        };

        if let Some(tokens) = &pane_obj.tokens {
            if tokens
                .grid_role
                .as_deref()
                .map(crate::cos::is_cos_pane_token)
                .unwrap_or(false)
            {
                return Err(format!(
                    "refusing to move CoS pane '{actual_pane}' (grid_role=cos)"
                ));
            }
        }

        if !is_candidate_worker(pane_obj) {
            return Err(format!(
                "apply: pane {actual_pane} is not a candidate worker"
            ));
        }

        // Finding 2: Recheck idle status before each move
        if pane_obj.agent_status.as_deref() == Some("working") {
            return Err(format!("pane {actual_pane} became busy; replan required"));
        }

        let current_tab = pane_obj.tab_id.clone();
        let worker_group = pane_obj
            .tokens
            .as_ref()
            .and_then(|t| t.grid_group.as_deref())
            .unwrap_or("workers");

        // Finding 5: Check if current tab is zoomed
        if let Some(ctab) = &current_tab {
            if let Some(layout) = fresh_snap.layouts.iter().find(|l| &l.tab_id == ctab) {
                if layout.zoomed == Some(true) {
                    return Err(format!("apply: tab {ctab} is zoomed"));
                }
            }
        }

        let remapped_target = match &op.target {
            Target::NewTab { label } => Target::NewTab {
                label: label.clone(),
            },
            Target::Split {
                tab,
                anchor,
                direction,
                ratio,
            } => {
                let actual_anchor = pane_map
                    .get(anchor)
                    .cloned()
                    .unwrap_or_else(|| anchor.clone());

                let anchor_pane = fresh_snap
                    .panes
                    .iter()
                    .find(|p| p.pane_id == actual_anchor)
                    .ok_or_else(|| {
                        format!("apply: anchor {actual_anchor} missing from snapshot")
                    })?;

                // Finding 5 & N1: Resolve @workers to the workers tab, NEVER the orchestrator tab
                let actual_tab = if tab == "@workers" || tab.starts_with("@workers-") {
                    let resolved = latest_tabs
                        .get(&(worker_group.to_string(), tab.clone()))
                        .cloned();

                    if let Some(r) = resolved {
                        r
                    } else {
                        // Look for an existing valid workers tab for this group,
                        // by the same acceptance predicate the plan and the
                        // repair use: the group tab, or the exact label the pane
                        // was spawned into (`grid_tab`).
                        let anchor_tid = anchor_pane.tab_id.as_deref().unwrap_or("");
                        let worker_gtab =
                            pane_obj.tokens.as_ref().and_then(|t| t.grid_tab.as_deref());
                        let is_workers_tab = fresh_snap
                            .tabs
                            .iter()
                            .find(|t| t.tab_id == anchor_tid)
                            .map(|t| {
                                super::plan::accepts_workers_tab(
                                    worker_group,
                                    worker_gtab,
                                    t.label.as_deref(),
                                )
                            })
                            .unwrap_or(false);

                        if is_workers_tab {
                            anchor_tid.to_string()
                        } else {
                            return Err(format!(
                                "apply: cannot resolve {tab} tab for group {worker_group}"
                            ));
                        }
                    }
                } else {
                    tab.clone()
                };

                // Finding 5: Target tab zoom verification
                if let Some(layout) = fresh_snap.layouts.iter().find(|l| l.tab_id == actual_tab) {
                    if layout.zoomed == Some(true) {
                        return Err(format!("apply: target tab {actual_tab} is zoomed"));
                    }
                }

                // Finding 5 remainder: Anchor is in the target tab check (apply.sh:209-213)
                let anchor_tab = anchor_pane.tab_id.as_deref().unwrap_or("");
                if anchor_tab != actual_tab {
                    return Err(format!(
                        "apply: anchor {actual_anchor} is in tab {anchor_tab}, expected target tab {actual_tab}"
                    ));
                }

                if let Some(atokens) = &anchor_pane.tokens {
                    if atokens.grid_role.as_deref() == Some("worker") {
                        let anchor_owner = atokens.grid_owner.as_deref().unwrap_or("");
                        let pane_owner = pane_obj
                            .tokens
                            .as_ref()
                            .and_then(|t| t.grid_owner.as_deref())
                            .unwrap_or("");
                        if anchor_owner != pane_owner {
                            return Err(format!(
                                "apply: anchor owner {anchor_owner} != pane owner {pane_owner}"
                            ));
                        }
                    }
                }

                Target::Split {
                    tab: actual_tab,
                    anchor: actual_anchor,
                    direction: direction.clone(),
                    ratio: *ratio,
                }
            }
        };

        // Findings 1, 5, 9: If current_tab == target_tab during split, park in scratch tab first
        let mut parked_pane: Option<String> = None;
        let effective_pane = if let Target::Split {
            tab: target_tab, ..
        } = &remapped_target
        {
            if current_tab.as_deref() == Some(target_tab.as_str()) {
                scratch_counter += 1;
                let scratch_label = format!(
                    "grid-scratch-{}-{}-{}",
                    worker_group,
                    std::process::id(),
                    scratch_counter
                );
                let park_res = driver.move_pane(
                    &actual_pane,
                    &Target::NewTab {
                        label: scratch_label,
                    },
                )?;
                parked_pane = Some(park_res.pane_id.clone());
                park_res.pane_id
            } else {
                actual_pane.clone()
            }
        } else {
            actual_pane.clone()
        };

        let res = driver.move_pane(&effective_pane, &remapped_target)?;

        let new_id = res.pane_id.clone();
        if let Some(prev) = &res.previous_pane_id {
            pane_map.insert(prev.clone(), new_id.clone());
        }
        pane_map.insert(op.pane.clone(), new_id.clone());
        pane_map.insert(actual_pane.clone(), new_id.clone());
        pane_map.insert(effective_pane.clone(), new_id.clone());
        if let Some(parked) = parked_pane {
            pane_map.insert(parked, new_id.clone());
        }

        if !res.tab_id.is_empty() && matches!(op.target, Target::NewTab { .. }) {
            let cnt = group_created_new_tab_count
                .entry(worker_group.to_string())
                .or_insert(0);
            let tab_ref = if *cnt == 0 { "@workers" } else { "@workers-2" };
            latest_tabs.insert(
                (worker_group.to_string(), tab_ref.to_string()),
                res.tab_id.clone(),
            );
            *cnt += 1;
        }
    }

    Ok(pane_map)
}
