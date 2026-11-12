use super::types::{Agent, Op, Pane, Rect, Snapshot, Target};
use std::collections::{BTreeSet, HashMap, HashSet};

const TOL: i64 = 2;

fn near(a: i64, b: i64) -> bool {
    (a - b).abs() <= TOL
}

fn rect_near(a: &Rect, b: &Rect) -> bool {
    near(a.x, b.x) && near(a.y, b.y) && near(a.width, b.width) && near(a.height, b.height)
}

pub fn pane_seq(pane_id: &str) -> u64 {
    let idx = match pane_id.rfind(":p") {
        Some(i) => i + 2,
        None => return 0,
    };
    let s = &pane_id[idx..];
    if s.is_empty() {
        return 0;
    }
    let alpha = "123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ0";
    let mut seq = 0u64;
    for c in s.chars() {
        let pos = match alpha.find(c) {
            Some(p) => (p as u64) + 1,
            None => 0,
        };
        seq = seq * 36 + pos;
    }
    seq
}

pub fn is_candidate_worker(pane: &Pane) -> bool {
    let tokens = match &pane.tokens {
        Some(t) => t,
        None => return false,
    };
    let ready = match &tokens.grid_ready {
        Some(serde_json::Value::String(s)) => s == "1",
        Some(serde_json::Value::Number(n)) => n.as_i64() == Some(1),
        _ => false,
    };
    if !ready {
        return false;
    }
    tokens.grid_role.as_deref() == Some("worker")
        && tokens
            .grid_owner
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
        && tokens
            .grid_group
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
}

fn is_status_deferred(status: Option<&str>, deferred_list: &[&str]) -> bool {
    status.map(|s| deferred_list.contains(&s)).unwrap_or(false)
}

pub fn is_group_tab(group: &str, label: Option<&str>) -> bool {
    let l = match label {
        Some(s) if !s.is_empty() => s,
        _ => return false,
    };
    l == group
        || l == format!("{group}-workers")
        || l == group.strip_suffix("-workers").unwrap_or(group)
        || l == "workers"
        || l.strip_suffix("-2")
            .map(|base| {
                base == group
                    || base == format!("{group}-workers")
                    || base == group.strip_suffix("-workers").unwrap_or(group)
                    || base == "workers"
            })
            .unwrap_or(false)
}

/// The one acceptance predicate for a group's workers tab, shared by the
/// planner (`plan`), the repair (`reconcile`) and the pre-prompt check
/// (`filter_ops_for_check`). A tab is the group's workers tab when its label is
/// the group tab (`is_group_tab`) or exactly the label the group's panes were
/// spawned into (`grid_tab`, i.e. GRID_TAB — which need not equal the group, e.g.
/// group `quaqua`, tab `quaqua-rs-workers`).
///
/// Widening this in one place is the point: two hand-mirrored widenings (the
/// planner's `ends_with("-workers")` vs the repair's `== requested`) disagreed on
/// the first foreign `<X>-workers` tab — the hook adopted it while the repair
/// moved the panes to its own tab, pulling them in opposite directions, and the
/// check (which tested only `is_group_tab`) dropped every op and was vacuous.
pub fn accepts_workers_tab(group: &str, expected: Option<&str>, label: Option<&str>) -> bool {
    if is_group_tab(group, label) {
        return true;
    }
    match (expected, label) {
        (Some(e), Some(l)) => {
            if e.is_empty() || l.is_empty() {
                return false;
            }
            if e == l {
                return true;
            }
            l.strip_suffix("-2") == Some(e) || e.strip_suffix("-2") == Some(l)
        }
        _ => false,
    }
}

pub fn chunk_tab_label(base_tab: &str, chunk_idx: usize) -> String {
    if chunk_idx == 0 {
        base_tab.to_string()
    } else {
        format!("{base_tab}-{}", chunk_idx + 1)
    }
}

pub fn chunk_tab_ref(chunk_idx: usize) -> String {
    if chunk_idx == 0 {
        "@workers".to_string()
    } else {
        format!("@workers-{}", chunk_idx + 1)
    }
}

pub fn accepts_chunk_tab(
    group: &str,
    expected_tab: Option<&str>,
    chunk_idx: usize,
    label: Option<&str>,
) -> bool {
    let l = match label {
        Some(s) if !s.is_empty() => s,
        _ => return false,
    };
    let base_tab = expected_tab.unwrap_or(group);
    let chunk_1_label = chunk_tab_label(base_tab, 1);
    let chunk_1_group_tab = format!("{group}-workers-2");

    if chunk_idx == 0 {
        if l == chunk_1_label || l == chunk_1_group_tab {
            return false;
        }
        accepts_workers_tab(group, expected_tab, Some(l))
    } else {
        if l == chunk_1_label || l == chunk_1_group_tab {
            return true;
        }
        if let Some(base_l) = l.strip_suffix("-2") {
            accepts_workers_tab(group, expected_tab, Some(base_l))
        } else {
            false
        }
    }
}

/// The workers tab label the group's panes were spawned into (GRID_TAB), read
/// from the pane tokens. `None` for legacy panes that carry no `grid_tab`; such
/// panes still match a group-derived label through `is_group_tab`.
pub fn group_expected_tab(gws: &[&Pane]) -> Option<String> {
    gws.iter().find_map(|w| {
        w.tokens
            .as_ref()
            .and_then(|t| t.grid_tab.as_deref())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

/// Target cells of one workers tab, in pane-sequence order (issue #79):
/// 2 = `A │ B`, 3 = `A │ B/C` (left full, right stacked), 4 = `A B / D C`.
pub fn cells(n: usize, a: &Rect) -> Vec<Rect> {
    let x = a.x;
    let y = a.y;
    let w = if a.width != 0 { a.width } else { 200 };
    let h = if a.height != 0 { a.height } else { 50 };
    let hw = w / 2;
    let hh = h / 2;
    match n {
        1 => vec![Rect {
            x,
            y,
            width: w,
            height: h,
        }],
        2 => vec![
            Rect {
                x,
                y,
                width: hw,
                height: h,
            },
            Rect {
                x: x + hw,
                y,
                width: w - hw,
                height: h,
            },
        ],
        3 => vec![
            Rect {
                x,
                y,
                width: hw,
                height: h,
            },
            Rect {
                x: x + hw,
                y,
                width: w - hw,
                height: hh,
            },
            Rect {
                x: x + hw,
                y: y + hh,
                width: w - hw,
                height: h - hh,
            },
        ],
        4 => vec![
            Rect {
                x,
                y,
                width: hw,
                height: hh,
            },
            Rect {
                x: x + hw,
                y,
                width: w - hw,
                height: hh,
            },
            Rect {
                x: x + hw,
                y: y + hh,
                width: w - hw,
                height: h - hh,
            },
            Rect {
                x,
                y: y + hh,
                width: hw,
                height: h - hh,
            },
        ],
        _ => vec![],
    }
}

/// Where the pane that lands at cell `pos` (0-based) comes from: the pane at
/// cell `parent` of the layout with `pos` panes splits in `dir` (issue #79).
/// Cell order is the incremental spawn order, so `pos` is also the number of
/// panes already in the tab.
pub fn placement(pos: usize) -> Option<(usize, &'static str)> {
    match pos {
        1 => Some((0, "right")),
        2 => Some((1, "down")),
        3 => Some((0, "down")),
        _ => None,
    }
}

/// Cell order == placement order: cell `i` is the `i`-th pane placed.
fn build_order(n: usize) -> Vec<usize> {
    (0..n).collect()
}

/// True when `rects` (one per pane, in any order) already covers the target
/// cells for `n` panes. Shape, not identity: a cleanup swap that leaves the
/// same geometry is converged, which keeps `grid check` and the repair agreeing.
pub fn layout_matches(n: usize, area: &Rect, rects: &[Option<Rect>]) -> bool {
    let want = cells(n, area);
    if want.is_empty() || rects.len() != want.len() {
        return false;
    }
    let mut used = vec![false; rects.len()];
    for cell in &want {
        match rects
            .iter()
            .enumerate()
            .find(|(i, r)| !used[*i] && r.map(|r| rect_near(&r, cell)).unwrap_or(false))
        {
            Some((i, _)) => used[i] = true,
            None => return false,
        }
    }
    true
}

/// True only when every pane rect is known and the shape is not the target —
/// a snapshot without layout data must not churn the repair forever.
pub fn layout_wrong(n: usize, area: &Rect, rects: &[Option<Rect>]) -> bool {
    rects.iter().all(|r| r.is_some()) && !layout_matches(n, area, rects)
}

/// Split for the pane placed at cell `pos`: anchored on the pane already at the
/// parent cell (`order[parent]`), in the parent's split direction.
fn split_target(pos: usize, order: &[usize], gws: &[&Pane], tab_ref: &str) -> Target {
    let (parent, direction) = placement(pos).expect("pos >= 1 has a parent cell");
    Target::Split {
        tab: tab_ref.to_string(),
        anchor: gws[order[parent]].pane_id.clone(),
        direction: direction.to_string(),
        ratio: 0.5,
    }
}

pub fn find_owner_pane(owner: &str, panes: &[Pane], agents: &[Agent], ws: &str) -> Option<String> {
    // The anchor must live in the same workspace as the panes it anchors:
    // a split into another workspace's tab cannot run, and an owner (`cos`)
    // can have a pane in several workspaces at once.
    let in_ws = |p: &Pane| ws.is_empty() || p.workspace_id.as_deref() == Some(ws);

    // 1. Pane tagged with matching grid_owner and non-worker role
    for p in panes {
        if !in_ws(p) {
            continue;
        }
        if let Some(t) = &p.tokens {
            if t.grid_owner.as_deref() == Some(owner) && t.grid_role.as_deref() != Some("worker") {
                return Some(p.pane_id.clone());
            }
        }
    }
    // 2. Registered agent whose name == owner
    for a in agents {
        if a.name.as_deref() == Some(owner) {
            if let Some(pid) = &a.pane_id {
                if panes.iter().any(|p| &p.pane_id == pid && in_ws(p)) {
                    return Some(pid.clone());
                }
            }
        }
    }
    // 3. Pane whose label == owner
    for p in panes {
        if !in_ws(p) {
            continue;
        }
        if p.label.as_deref() == Some(owner) {
            return Some(p.pane_id.clone());
        }
    }
    // 4. Pane whose agent == owner
    for p in panes {
        if !in_ws(p) {
            continue;
        }
        if p.agent.as_deref() == Some(owner) {
            return Some(p.pane_id.clone());
        }
    }
    // 5. For cos only: pane tagged orchestrator/lead/cos or first non-worker pane
    if owner == "cos" {
        for p in panes {
            if in_ws(p) {
                if let Some(t) = &p.tokens {
                    let role = t.grid_role.as_deref().unwrap_or("");
                    if role == "orchestrator" || role == "cos" || role == "lead" {
                        return Some(p.pane_id.clone());
                    }
                }
            }
        }
        for p in panes {
            if in_ws(p) && !is_candidate_worker(p) {
                return Some(p.pane_id.clone());
            }
        }
    }
    None
}

/// A split whose anchor is deferred cannot run, and a split into `@workers`
/// cannot run while the group's new tab is deferred (there is nowhere to land).
/// The apply path and the hook both propagate that through this same rule;
/// the planner must too, or the check counts an unrunnable op as pending and
/// times out while its anchor is still working.
pub(crate) fn propagate_deferrals(ops: &mut [Op], panes: &[Pane]) {
    let pane_group = |pid: &str| -> String {
        panes
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

    for op in ops.iter_mut() {
        let grp = pane_group(&op.pane);
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
            if deferred_panes.contains(anchor.as_str()) || tab_def {
                op.deferred = true;
                deferred_panes.insert(op.pane.clone());
            }
        }
    }
}

/// Pure layout planner: turns a snapshot into an ordered list of layout ops.
pub fn plan(snapshot: &Snapshot) -> Result<Vec<Op>, String> {
    plan_with_deferred(snapshot, None)
}

/// Layout planner with custom deferred statuses from config.
pub fn plan_with_deferred(
    snapshot: &Snapshot,
    deferred_statuses: Option<&[&str]>,
) -> Result<Vec<Op>, String> {
    let def_list = deferred_statuses.unwrap_or(&["working"]);
    let candidate_workers: Vec<&Pane> = snapshot
        .panes
        .iter()
        .filter(|p| is_candidate_worker(p))
        .collect();

    if candidate_workers.is_empty() {
        return Ok(vec![]);
    }
    // Distinct owners in sorted order (jq: `[ $candidate_workers[].tokens.grid_owner ] | unique`)
    let mut owners: BTreeSet<&str> = BTreeSet::new();
    for p in &candidate_workers {
        if let Some(t) = &p.tokens {
            if let Some(o) = &t.grid_owner {
                owners.insert(o.as_str());
            }
        }
    }

    let mut ops = Vec::new();

    for owner in owners {
        let mut owner_workers: Vec<&Pane> = candidate_workers
            .iter()
            .copied()
            .filter(|p| p.tokens.as_ref().and_then(|t| t.grid_owner.as_deref()) == Some(owner))
            .collect();

        owner_workers.sort_by(|a, b| {
            let seq_a = pane_seq(&a.pane_id);
            let seq_b = pane_seq(&b.pane_id);
            seq_a.cmp(&seq_b).then_with(|| a.pane_id.cmp(&b.pane_id))
        });

        // A tab lives in exactly one workspace and a pane is only ever moved
        // inside its own workspace, so an owner's panes are arranged per
        // workspace: grouping them by owner alone made one owner's panes in
        // workspace B targets of a tab in workspace A — a move herdr cannot
        // perform, so the op never converged and `grid check` timed out. One
        // owner really does span workspaces: `cos` is the default tag, so every
        // pane whose shell lost its `QUAQUA_OWNER` (a herdr restart restores the
        // daemon env) lands under it, across every project workspace.
        let mut workspaces: BTreeSet<&str> = BTreeSet::new();
        for w in &owner_workers {
            workspaces.insert(w.workspace_id.as_deref().unwrap_or(""));
        }

        for owner_ws in workspaces {
            let ws_workers: Vec<&Pane> = owner_workers
                .iter()
                .copied()
                .filter(|w| w.workspace_id.as_deref().unwrap_or("") == owner_ws)
                .collect();
            plan_owner_workspace(owner, owner_ws, &ws_workers, snapshot, def_list, &mut ops)?;
        }
    }

    propagate_deferrals(&mut ops, &snapshot.panes);
    Ok(ops)
}

/// Plan one `(owner, workspace)` cell of the grid: the owner's panes that live
/// in `owner_ws`, arranged beside the owner's pane or into `owner_ws`'s workers
/// tab. Every op it emits targets either a tab of `owner_ws` or a new tab, which
/// herdr creates in the pane's own workspace.
fn plan_owner_workspace(
    owner: &str,
    owner_ws: &str,
    owner_workers: &[&Pane],
    snapshot: &Snapshot,
    def_list: &[&str],
    ops: &mut Vec<Op>,
) -> Result<(), String> {
    let total = owner_workers.len();
    if total == 0 {
        return Ok(());
    }

    let anchor_pane_id = find_owner_pane(owner, &snapshot.panes, &snapshot.agents, owner_ws);
    let anchor_pane = anchor_pane_id
        .as_ref()
        .and_then(|id| snapshot.panes.iter().find(|p| &p.pane_id == id));
    let anchor_tab = anchor_pane.and_then(|p| p.tab_id.as_deref());

    if let (1, Some(anchor_pane)) = (total, anchor_pane) {
        let w = owner_workers[0];
        // "Beside the anchor" is acceptance, not a target ratio: the pane is
        // arranged as soon as it shares the anchor's tab. The repair path
        // (reconcile.rs `beside`) uses exactly this test, so the check and the
        // repair must not disagree — a herdr `pane split` lands 50/50, never
        // the old 2/5|3/5, and the old rect_near test made `--check` time out
        // forever on an already-arranged pane.
        let atab = anchor_tab.unwrap_or_default();
        let in_place = anchor_tab.is_some() && w.tab_id.as_deref() == Some(atab);

        if !in_place {
            ops.push(Op {
                op: "move".to_string(),
                pane: w.pane_id.clone(),
                deferred: is_status_deferred(w.agent_status.as_deref(), def_list),
                target: Target::Split {
                    tab: atab.to_string(),
                    anchor: anchor_pane.pane_id.clone(),
                    direction: "right".to_string(),
                    ratio: 0.4,
                },
            });
        }
    } else {
        // >1 workers total for this owner -> one tab per group
        // Distinct groups in sorted order (jq: `[ $owner_workers[].tokens.grid_group ] | unique`)
        let mut groups: BTreeSet<&str> = BTreeSet::new();
        for w in owner_workers {
            if let Some(t) = &w.tokens {
                if let Some(g) = &t.grid_group {
                    groups.insert(g.as_str());
                }
            }
        }

        for group in groups {
            let mut gws: Vec<&Pane> = owner_workers
                .iter()
                .copied()
                .filter(|w| w.tokens.as_ref().and_then(|t| t.grid_group.as_deref()) == Some(group))
                .collect();

            gws.sort_by(|a, b| {
                let seq_a = pane_seq(&a.pane_id);
                let seq_b = pane_seq(&b.pane_id);
                seq_a.cmp(&seq_b).then_with(|| a.pane_id.cmp(&b.pane_id))
            });

            let n = gws.len();
            if n > 8 {
                return Err(format!("too many workers in group {group}: {n} (max 8)"));
            }

            // The exact workers tab label this group was spawned into
            // (GRID_TAB), shared with the repair and the check through
            // `accepts_workers_tab`.
            let expected_tab = group_expected_tab(&gws);
            let base_tab = expected_tab.clone().unwrap_or_else(|| group.to_string());

            let chunks: Vec<&[&Pane]> = if n <= 4 {
                vec![&gws[..]]
            } else {
                vec![&gws[..4], &gws[4..]]
            };

            for (chunk_idx, chunk_gws) in chunks.into_iter().enumerate() {
                let m = chunk_gws.len();
                if m == 0 {
                    continue;
                }

                let chunk_label = chunk_tab_label(&base_tab, chunk_idx);
                let tab_ref = chunk_tab_ref(chunk_idx);

                // Search for an existing valid workers tab for this group
                // jq:
                // [ $tabs[]
                //   | select((.workspace_id == $owner_ws or $owner_ws == "")
                //            and ($anchor_tab == null or .tab_id != $anchor_tab)
                //            and accepts_chunk_tab($group; $expected; $chunk_idx; .label))
                //   | .tab_id as $tid
                //   | [ $panes[] | select(.tab_id == $tid) ] as $occ
                //   | select(($occ | length > 0)
                //            and ($occ | all(. as $p | any($gws[]; .pane_id == $p.pane_id))))
                // ]
                // | sort_by(if .label == $chunk_label then 0 else 1 end)
                // | first // null
                let mut matching_tabs: Vec<&super::types::Tab> = snapshot
                    .tabs
                    .iter()
                    .filter(|tab| {
                        let ws_ok =
                            tab.workspace_id.as_deref() == Some(owner_ws) || owner_ws.is_empty();
                        let anchor_ok = anchor_tab.map(|at| tab.tab_id != at).unwrap_or(true);
                        let label_ok = accepts_chunk_tab(
                            group,
                            expected_tab.as_deref(),
                            chunk_idx,
                            tab.label.as_deref(),
                        );
                        if !(ws_ok && anchor_ok && label_ok) {
                            return false;
                        }
                        let occ: Vec<&Pane> = snapshot
                            .panes
                            .iter()
                            .filter(|p| p.tab_id.as_deref() == Some(&tab.tab_id))
                            .collect();
                        if occ.is_empty() {
                            return false;
                        }
                        occ.iter()
                            .all(|p| gws.iter().any(|gw| gw.pane_id == p.pane_id))
                    })
                    .collect();

                matching_tabs.sort_by_key(|tab| {
                    let l = tab.label.as_deref().unwrap_or("");
                    let exact = l == chunk_label;
                    if exact {
                        0
                    } else {
                        1
                    }
                });

                let valid_tab = matching_tabs.first().copied();

                if let Some(vt) = valid_tab {
                    let wt_id = &vt.tab_id;
                    let wt_layout = snapshot.layouts.iter().find(|l| &l.tab_id == wt_id);
                    let wt_area = wt_layout.and_then(|l| l.area).unwrap_or(Rect {
                        x: 0,
                        y: 0,
                        width: 200,
                        height: 50,
                    });
                    let occ_count = snapshot
                        .panes
                        .iter()
                        .filter(|p| p.tab_id.as_deref() == Some(wt_id.as_str()))
                        .count();
                    let all_present = chunk_gws
                        .iter()
                        .all(|w| w.tab_id.as_deref() == Some(wt_id.as_str()))
                        && occ_count == m;

                    let in_place = if all_present {
                        let rects: Vec<Option<Rect>> = chunk_gws
                            .iter()
                            .map(|w| {
                                wt_layout.and_then(|l| {
                                    l.panes
                                        .iter()
                                        .find(|p| p.pane_id == w.pane_id)
                                        .and_then(|p| p.rect)
                                })
                            })
                            .collect();
                        // Unknown layout data must not read as "wrong" (a
                        // repair would plan a rebuild on every pass forever).
                        !layout_wrong(m, &wt_area, &rects)
                    } else {
                        false
                    };

                    if !in_place {
                        let in_tab_count = chunk_gws
                            .iter()
                            .filter(|w| w.tab_id.as_deref() == Some(wt_id.as_str()))
                            .count();
                        // All panes are there but the shape is wrong (a cleanup
                        // left the mirror column, or a mirror-image 2x2): rebuild
                        // in cell order. `apply_ops` parks each pane in a scratch
                        // tab first, so the split lands on the parent cell.
                        let rebuild = all_present;
                        if in_tab_count > 0
                            && chunk_gws[0].tab_id.as_deref() == Some(wt_id.as_str())
                        {
                            // Worker 0 is already in tab; place missing workers,
                            // or re-place every pane when rebuilding the shape.
                            let order = build_order(m);
                            for (pos, i) in order.iter().copied().enumerate() {
                                if pos == 0 {
                                    continue;
                                }
                                let here = chunk_gws[i].tab_id.as_deref() == Some(wt_id.as_str());
                                if here && !rebuild {
                                    continue;
                                }
                                ops.push(Op {
                                    op: "move".to_string(),
                                    pane: chunk_gws[i].pane_id.clone(),
                                    deferred: is_status_deferred(
                                        chunk_gws[i].agent_status.as_deref(),
                                        def_list,
                                    ),
                                    target: split_target(pos, &order, chunk_gws, wt_id),
                                });
                            }
                        } else {
                            // The workers tab exists but does not hold the
                            // first-spawned worker of this chunk: place the missing workers into
                            // that tab instead of creating a second one.
                            let anchor = chunk_gws
                                .iter()
                                .find(|w| w.tab_id.as_deref() == Some(wt_id.as_str()))
                                .map(|w| w.pane_id.clone())
                                .unwrap_or_else(|| chunk_gws[0].pane_id.clone());
                            for i in build_order(m) {
                                if chunk_gws[i].tab_id.as_deref() == Some(wt_id.as_str()) {
                                    continue;
                                }
                                ops.push(Op {
                                    op: "move".to_string(),
                                    pane: chunk_gws[i].pane_id.clone(),
                                    deferred: is_status_deferred(
                                        chunk_gws[i].agent_status.as_deref(),
                                        def_list,
                                    ),
                                    target: Target::Split {
                                        tab: wt_id.to_string(),
                                        anchor: anchor.clone(),
                                        direction: "right".to_string(),
                                        ratio: 0.5,
                                    },
                                });
                            }
                        }
                    }
                } else {
                    // No valid workers tab exists -> create new tab and split.
                    // Label the tab with the pane's spawn label when it has one
                    // (`expected_tab`), so the hook and the repair create the
                    // same tab instead of fighting over `group` vs `--tab`.
                    let order = build_order(m);
                    for (pos, i) in order.iter().copied().enumerate() {
                        if pos == 0 {
                            ops.push(Op {
                                op: "move".to_string(),
                                pane: chunk_gws[i].pane_id.clone(),
                                deferred: is_status_deferred(
                                    chunk_gws[i].agent_status.as_deref(),
                                    def_list,
                                ),
                                target: Target::NewTab {
                                    label: chunk_label.clone(),
                                },
                            });
                        } else {
                            ops.push(Op {
                                op: "move".to_string(),
                                pane: chunk_gws[i].pane_id.clone(),
                                deferred: is_status_deferred(
                                    chunk_gws[i].agent_status.as_deref(),
                                    def_list,
                                ),
                                target: split_target(pos, &order, chunk_gws, &tab_ref),
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::types::{Layout, PaneLayout, Tab, Tokens};

    const AREA: Rect = Rect {
        x: 0,
        y: 0,
        width: 200,
        height: 100,
    };

    /// A binary split tree with herdr's `pane move --split` semantics: a split
    /// puts the moved pane below/right of the anchor leaf, and removing a leaf
    /// collapses its parent onto the sibling. That is what `apply_ops` does
    /// (park in a scratch tab, then split) and what the isolated smoke observed.
    #[derive(Clone, Debug)]
    enum Node {
        Leaf(String),
        Split {
            dir: &'static str,
            a: Box<Node>,
            b: Box<Node>,
        },
    }

    impl Node {
        fn split(&mut self, anchor: &str, dir: &'static str, new: &str) -> bool {
            match self {
                Node::Leaf(p) if p == anchor => {
                    let old = Node::Leaf(p.clone());
                    *self = Node::Split {
                        dir,
                        a: Box::new(old),
                        b: Box::new(Node::Leaf(new.to_string())),
                    };
                    true
                }
                Node::Leaf(_) => false,
                Node::Split { a, b, .. } => a.split(anchor, dir, new) || b.split(anchor, dir, new),
            }
        }

        fn remove(&mut self, pane: &str) -> bool {
            match self {
                Node::Leaf(_) => false,
                Node::Split { a, b, .. } => {
                    if matches!(a.as_ref(), Node::Leaf(p) if p == pane) {
                        *self = b.as_ref().clone();
                        return true;
                    }
                    if matches!(b.as_ref(), Node::Leaf(p) if p == pane) {
                        *self = a.as_ref().clone();
                        return true;
                    }
                    a.remove(pane) || b.remove(pane)
                }
            }
        }

        fn rects(&self, area: Rect, out: &mut Vec<(String, Rect)>) {
            match self {
                Node::Leaf(p) => out.push((p.clone(), area)),
                Node::Split { dir, a, b } => {
                    if *dir == "right" {
                        let hw = area.width / 2;
                        a.rects(Rect { width: hw, ..area }, out);
                        b.rects(
                            Rect {
                                x: area.x + hw,
                                width: area.width - hw,
                                ..area
                            },
                            out,
                        );
                    } else {
                        let hh = area.height / 2;
                        a.rects(Rect { height: hh, ..area }, out);
                        b.rects(
                            Rect {
                                y: area.y + hh,
                                height: area.height - hh,
                                ..area
                            },
                            out,
                        );
                    }
                }
            }
        }
    }

    fn worker(id: &str, tab: &str) -> Pane {
        Pane {
            pane_id: id.to_string(),
            tab_id: Some(tab.to_string()),
            workspace_id: Some("w1".to_string()),
            agent: Some(id.to_string()),
            agent_status: Some("idle".to_string()),
            tokens: Some(Tokens {
                grid_role: Some("worker".to_string()),
                grid_owner: Some("cos".to_string()),
                grid_group: Some("grp".to_string()),
                grid_tab: Some("grp-workers".to_string()),
                grid_ready: Some(serde_json::json!(1)),
                grid_name: Some(id.to_string()),
            }),
            label: Some(id.to_string()),
        }
    }

    fn owner() -> Pane {
        Pane {
            pane_id: "w1:p1".to_string(),
            tab_id: Some("w1:t1".to_string()),
            workspace_id: Some("w1".to_string()),
            agent: Some("cos".to_string()),
            agent_status: Some("idle".to_string()),
            tokens: Some(Tokens {
                grid_role: Some("cos".to_string()),
                grid_owner: Some("cos".to_string()),
                grid_group: None,
                grid_tab: None,
                grid_ready: None,
                grid_name: Some("cos".to_string()),
            }),
            label: Some("cos".to_string()),
        }
    }

    /// Snapshot: `tree` is the workers tab, `extra` panes sit in the
    /// orchestrator tab (a fresh spawn before the repair moves it).
    fn snapshot(tree: &Node, extra: &[&str]) -> Snapshot {
        let mut panes = vec![owner()];
        let mut pane_layouts = Vec::new();
        let mut all = Vec::new();
        tree.rects(AREA, &mut all);
        for (id, rect) in all {
            panes.push(worker(&id, "w1:tw"));
            pane_layouts.push(PaneLayout {
                pane_id: id,
                rect: Some(rect),
            });
        }
        for id in extra {
            panes.push(worker(id, "w1:t1"));
        }
        Snapshot {
            tabs: vec![
                Tab {
                    tab_id: "w1:t1".to_string(),
                    workspace_id: Some("w1".to_string()),
                    label: Some("1".to_string()),
                    pane_count: None,
                },
                Tab {
                    tab_id: "w1:tw".to_string(),
                    workspace_id: Some("w1".to_string()),
                    label: Some("grp-workers".to_string()),
                    pane_count: None,
                },
            ],
            layouts: vec![Layout {
                tab_id: "w1:tw".to_string(),
                workspace_id: Some("w1".to_string()),
                area: Some(AREA),
                panes: pane_layouts,
                zoomed: Some(false),
            }],
            panes,
            ..Default::default()
        }
    }

    fn apply(tree: &mut Node, op: &Op) {
        tree.remove(&op.pane);
        match &op.target {
            Target::Split {
                anchor, direction, ..
            } => assert!(
                tree.split(
                    anchor,
                    if direction == "right" {
                        "right"
                    } else {
                        "down"
                    },
                    &op.pane
                ),
                "anchor {anchor} missing for {}",
                op.pane
            ),
            Target::NewTab { .. } => panic!("unexpected new_tab in a rebuild"),
        }
    }

    fn matches(tree: &Node, n: usize) -> bool {
        let mut all = Vec::new();
        tree.rects(AREA, &mut all);
        let rects: Vec<Option<Rect>> = all.into_iter().map(|(_, r)| Some(r)).collect();
        layout_matches(n, &AREA, &rects)
    }

    /// Issue #79 acceptance: four sequential spawns end in an equal 2x2.
    #[test]
    fn test_four_sequential_spawns_give_equal_2x2() {
        // Spawn 1: A arrives into the (still empty) workers tab.
        let mut tree = Node::Leaf("w1:p2".to_string());
        assert!(matches(&tree, 1));

        // Spawns 2..4: each new pane lands in the orchestrator tab and the
        // repair moves it into the workers tab.
        let wanted = ["right", "down", "down"];
        for (k, dir) in wanted.iter().enumerate() {
            let n = k + 2;
            let new_pane = format!("w1:p{}", n + 1);
            let snap = snapshot(&tree, &[&new_pane]);
            let ops = plan(&snap).unwrap();
            let op = ops
                .iter()
                .find(|o| o.pane == new_pane)
                .unwrap_or_else(|| panic!("spawn {n}: no move for {new_pane}: {ops:?}"));
            match &op.target {
                Target::Split { direction, .. } => assert_eq!(direction, dir, "spawn {n}: {op:?}"),
                other => panic!("spawn {n}: expected a split, got {other:?}"),
            }
            apply(&mut tree, op);
            assert!(
                matches(&tree, n),
                "spawn {n}: not the target shape: {tree:?}"
            );
            // The repaired tab is converged: a second pass plans nothing.
            let after = snapshot(&tree, &[]);
            assert_eq!(
                plan(&after).unwrap(),
                vec![],
                "spawn {n}: replan must converge"
            );
        }

        let mut all = Vec::new();
        tree.rects(AREA, &mut all);
        let rects: Vec<Rect> = all.into_iter().map(|(_, r)| r).collect();
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert_eq!(a.width, b.width, "2x2 columns must be equal: {rects:?}");
                assert_eq!(a.height, b.height, "2x2 rows must be equal: {rects:?}");
            }
        }
    }

    /// A 4-pane tab as it is after the sequential spawns above.
    fn four_pane_tree() -> Node {
        let mut tree = Node::Leaf("w1:p2".to_string()); // A
        tree.split("w1:p2", "right", "w1:p3"); // B
        tree.split("w1:p3", "down", "w1:p4"); // C (bottom-right)
        tree.split("w1:p2", "down", "w1:p5"); // D (bottom-left)
        assert!(matches(&tree, 4));
        tree
    }

    /// Cleanup table, row 4->3: removing a left-column pane needs no fix;
    /// removing a right-column pane is the mirror and must be repaired.
    #[test]
    fn test_cleanup_4_to_3() {
        for removed in ["w1:p2", "w1:p5"] {
            let mut tree = four_pane_tree();
            tree.remove(removed);
            let snap = snapshot(&tree, &[]);
            let ops = plan(&snap).unwrap();
            assert_eq!(
                ops,
                vec![],
                "4->3 removing left pane {removed} needs no fix: {ops:?}"
            );
            assert!(matches(&tree, 3));
        }

        for removed in ["w1:p3", "w1:p4"] {
            let mut tree = four_pane_tree();
            tree.remove(removed);
            assert!(
                !matches(&tree, 3),
                "4->3 removing right pane {removed} starts mirrored"
            );
            let snap = snapshot(&tree, &[]);
            let ops = plan(&snap).unwrap();
            assert!(
                !ops.is_empty(),
                "4->3 removing {removed}: expected a swap/rebuild"
            );
            for op in &ops {
                apply(&mut tree, op);
            }
            assert!(
                matches(&tree, 3),
                "4->3 removing {removed}: not repaired: {tree:?}"
            );
        }
    }

    /// Cleanup table, row 3->2.
    #[test]
    fn test_cleanup_3_to_2() {
        // Removing B or C leaves A | X: already the target.
        for removed in ["w1:p3", "w1:p4"] {
            let mut tree = four_pane_tree();
            tree.remove("w1:p5"); // D, first: left full + right stacked
            tree.remove(removed);
            assert!(matches(&tree, 2), "3->2 removing {removed} needs no fix");
            let ops = plan(&snapshot(&tree, &[])).unwrap();
            assert_eq!(ops, vec![], "3->2 removing {removed}: {ops:?}");
        }

        // Removing A leaves the right column stacked: move C right of B.
        let mut tree = four_pane_tree();
        tree.remove("w1:p5");
        tree.remove("w1:p2");
        assert!(!matches(&tree, 2), "3->2 removing A starts stacked");
        let ops = plan(&snapshot(&tree, &[])).unwrap();
        assert_eq!(ops.len(), 1, "one move fixes 3->2: {ops:?}");
        match &ops[0].target {
            Target::Split {
                anchor, direction, ..
            } => {
                assert_eq!(anchor, "w1:p3", "C splits right of B");
                assert_eq!(direction, "right");
            }
            other => panic!("expected a split, got {other:?}"),
        }
        apply(&mut tree, &ops[0]);
        assert!(matches(&tree, 2), "3->2 removing A: {tree:?}");
    }
}
