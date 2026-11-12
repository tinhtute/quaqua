use super::apply::{apply_ops, CliHerdrDriver, HerdrDriver};
use super::plan::{
    accepts_workers_tab, group_expected_tab, is_candidate_worker, plan_with_deferred,
};
use super::types::{Op, Snapshot, Target};
use crate::engine::activity::{
    Activity, ActivityError, ActivityRequest, ActivityResult, GridRequest, GridResult,
};
use std::collections::HashSet;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ReconcileOptions {
    pub check: bool,
    pub repair: bool,
    pub tab: Option<String>,
    pub only: Option<String>,
    pub owner: Option<String>,
    pub dry_run: bool,
    pub timeout_sec: Option<u64>,
}

impl Default for ReconcileOptions {
    fn default() -> Self {
        ReconcileOptions {
            check: false,
            repair: true,
            tab: None,
            only: None,
            owner: None,
            dry_run: false,
            timeout_sec: None,
        }
    }
}

pub use crate::util::now_sec;

fn poll_interval() -> Duration {
    if let Ok(poll_s) = std::env::var("GRID_CHECK_POLL_SEC") {
        if let Ok(s) = poll_s.trim().parse::<f64>() {
            if s > 0.0 {
                return Duration::from_secs_f64(s);
            }
        }
    }
    Duration::from_millis(500)
}

pub fn filter_ops_for_check<'a>(
    ops: &'a [Op],
    snapshot: &Snapshot,
    owner_filter: Option<&str>,
    only_filter: Option<&str>,
    tab_filter: Option<&str>,
    workspace_filter: Option<&str>,
) -> Vec<&'a Op> {
    let only_set: Option<HashSet<&str>> = only_filter.map(|s| {
        s.split(',')
            .map(|x| x.trim())
            .filter(|x| !x.is_empty())
            .collect()
    });

    ops.iter()
        .filter(|op| {
            if op.deferred {
                return false;
            }

            let pane = snapshot.panes.iter().find(|p| p.pane_id == op.pane);
            let agent = snapshot
                .agents
                .iter()
                .find(|a| a.pane_id.as_deref() == Some(&op.pane));

            let name = agent
                .and_then(|a| a.name.as_deref())
                .or_else(|| {
                    pane.and_then(|p| p.tokens.as_ref())
                        .and_then(|t| t.grid_name.as_deref())
                })
                .or_else(|| pane.and_then(|p| p.label.as_deref()))
                .unwrap_or("");

            // Only the invoker's own workspace is in scope: a tab in another
            // workspace can never receive these panes, so counting such an op
            // as pending is what timed the check out (the `wA:p1J -> new_tab
            // quaqua-workers` move of a Lead's pane into the CoS workspace).
            // A pane with no recorded workspace is left in scope.
            if let Some(ws) = workspace_filter {
                if !ws.is_empty() {
                    if let Some(pw) = pane.and_then(|p| p.workspace_id.as_deref()) {
                        if pw != ws {
                            return false;
                        }
                    }
                }
            }

            let powner = pane
                .and_then(|p| p.tokens.as_ref())
                .and_then(|t| t.grid_owner.as_deref())
                .unwrap_or("cos");
            let pgroup = pane
                .and_then(|p| p.tokens.as_ref())
                .and_then(|t| t.grid_group.as_deref())
                .unwrap_or("");
            let pgtab = pane
                .and_then(|p| p.tokens.as_ref())
                .and_then(|t| t.grid_tab.as_deref());

            if let Some(req_owner) = owner_filter {
                if !req_owner.is_empty() && powner != req_owner {
                    return false;
                }
            }

            if let Some(set) = &only_set {
                if !set.is_empty() && !set.contains(name) {
                    return false;
                }
            }

            // dec5a30 bugfix: --tab always scopes the check, with or without --only.
            // Same acceptance predicate the planner and the repair use: the
            // requested tab is accepted when it is the group tab or the exact
            // label the pane was spawned into (`grid_tab`). Testing only
            // `is_group_tab` made the check vacuous whenever GRID_TAB differed
            // from the group token (group `quaqua`, tab `quaqua-rs-workers`):
            // every op was dropped, so `--check` returned Ok on any layout.
            //
            // The chunk filter must bound both chunks, not only the overflow:
            // with `--tab quaqua-workers` every pane of the group passes
            // `accepts_workers_tab`, so an unfiltered chunk-1 op moved panes in
            // `quaqua-workers-2` while the caller checked `quaqua-workers`.
            if let Some(tab) = tab_filter {
                if !tab.is_empty() {
                    if !accepts_workers_tab(pgroup, pgtab, Some(tab)) {
                        return false;
                    }
                    let base = pgtab.unwrap_or(pgroup);
                    let is_chunk_1_filter = tab == super::plan::chunk_tab_label(base, 1)
                        || tab == format!("{pgroup}-workers-2");
                    let chunk_1_labels = [
                        super::plan::chunk_tab_label(base, 1),
                        format!("{pgroup}-workers-2"),
                    ];
                    if is_chunk_1_filter {
                        let op_matches = match &op.target {
                            Target::NewTab { label } => label == tab,
                            Target::Split {
                                tab: target_tab, ..
                            } => {
                                if target_tab == "@workers-2" {
                                    true
                                } else {
                                    snapshot.tabs.iter().any(|t| {
                                        t.tab_id == *target_tab && t.label.as_deref() == Some(tab)
                                    })
                                }
                            }
                        };
                        if !op_matches {
                            return false;
                        }
                    } else {
                        // Reciprocal of the chunk-1 filter: a check scoped to
                        // the base tab must not apply an op that targets the
                        // overflow tab. Without this, `--tab quaqua-workers`
                        // accepted every pane of the group and moved panes in
                        // `quaqua-workers-2` while checking `quaqua-workers`.
                        let target_is_chunk_1 = match &op.target {
                            Target::NewTab { label } => chunk_1_labels.iter().any(|l| l == label),
                            Target::Split {
                                tab: target_tab, ..
                            } => {
                                if target_tab == "@workers-2" || target_tab == "@workers" {
                                    target_tab == "@workers-2"
                                } else {
                                    snapshot.tabs.iter().any(|t| {
                                        t.tab_id == *target_tab
                                            && t.label
                                                .as_deref()
                                                .map(|l| chunk_1_labels.iter().any(|c| c == l))
                                                .unwrap_or(false)
                                    })
                                }
                            }
                        };
                        if target_is_chunk_1 {
                            return false;
                        }
                    }
                }
            }

            true
        })
        .collect()
}

/// True when `op` re-places a pane into a workers tab whose current shape is
/// wrong while another op into that same tab is deferred (its pane is
/// `working`).
///
/// A rebuild re-places the panes of a full workers tab in cell order and is
/// atomic: while a deferred pane still has to move, an idle pane's rebuild op
/// cannot reach its cell, and applying it only repeats the same op on every
/// poll (the live 4-pane vertical stack: pB idle, p9/pA working). Such an op
/// must not fail the check; a later spawn/cleanup/prompt heals it once the
/// deferred panes idle.
fn blocked_by_deferred_rebuild(snap: &Snapshot, ops: &[Op], op: &Op) -> bool {
    let Target::Split { tab, .. } = &op.target else {
        return false;
    };
    // `@workers*` is resolved by apply_ops; leave those to the apply path.
    if tab.starts_with('@') {
        return false;
    }
    let layout = match snap.layouts.iter().find(|l| &l.tab_id == tab) {
        Some(l) => l,
        None => return false,
    };
    let occupants: Vec<&super::types::Pane> = snap
        .panes
        .iter()
        .filter(|p| p.tab_id.as_deref() == Some(tab.as_str()))
        .collect();
    let m = occupants.len();
    if m < 2 {
        return false;
    }
    let area = layout.area.unwrap_or(super::types::Rect {
        x: 0,
        y: 0,
        width: 200,
        height: 50,
    });
    let rects: Vec<Option<super::types::Rect>> = occupants
        .iter()
        .map(|p| {
            layout
                .panes
                .iter()
                .find(|pl| pl.pane_id == p.pane_id)
                .and_then(|pl| pl.rect)
        })
        .collect();
    if !super::plan::layout_wrong(m, &area, &rects) {
        return false;
    }
    let group = |pane_id: &str| {
        snap.panes
            .iter()
            .find(|p| p.pane_id == pane_id)
            .and_then(|p| p.tokens.as_ref())
            .and_then(|t| t.grid_group.as_deref())
    };
    let op_group = group(&op.pane);
    ops.iter().any(|other| {
        other.deferred
            && group(&other.pane) == op_group
            && matches!(&other.target, Target::Split { tab: t, .. } if t == tab)
    })
}

/// True for the tag that names a project team's Lead (`<slug>-lead`). Such an
/// owner is specific; `cos` is the default a pane falls back to when its shell
/// lost the variable, and a herdr restart restores panes with the daemon's env.
pub fn is_project_owner(owner: &str) -> bool {
    owner
        .strip_suffix("-lead")
        .map(|slug| !slug.is_empty())
        .unwrap_or(false)
}

/// The pane that invoked this process. herdr exports `HERDR_PANE_ID` to every
/// process it starts; only the snapshot knows that pane's tokens.
fn caller_pane(snap: &Snapshot) -> Option<&super::types::Pane> {
    let id = std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|s| !s.is_empty())?;
    snap.panes.iter().find(|p| p.pane_id == id)
}

/// The owner a pane is tagged with, when it is not itself a worker: a Lead
/// keeps its identity in pane metadata, which outlives the shell environment.
fn pane_owner(pane: &super::types::Pane) -> Option<&str> {
    let t = pane.tokens.as_ref()?;
    if t.grid_role.as_deref() == Some("worker") {
        return None;
    }
    t.grid_owner.as_deref().filter(|o| !o.is_empty())
}

/// Owner and workspace a reconcile call acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridScope {
    pub owner: String,
    pub workspace: Option<String>,
}

/// Resolve what one reconcile call may touch.
///
/// Owner: an explicit `--owner` wins; otherwise a project Lead owner beats the
/// ambient/default one whichever side it is on, and the caller's tagged pane
/// beats `$QUAQUA_OWNER` (a Lead's shell can carry the CoS value while its pane
/// still says `<slug>-lead`).
///
/// Workspace: the caller's own workspace, else the workspace of the requested
/// workers tab when exactly one tab carries that label. Panes are arranged one
/// workspace at a time regardless (see `plan::plan_owner_workspace`); this only
/// keeps one project's repair from touching another project's panes.
pub fn grid_scope(
    snap: &Snapshot,
    requested_owner: Option<&str>,
    requested_tab: Option<&str>,
) -> GridScope {
    let caller = caller_pane(snap);
    let caller_owner = caller.and_then(pane_owner);
    let env_owner = std::env::var("QUAQUA_OWNER").ok().filter(|s| !s.is_empty());

    let owner = match requested_owner.filter(|o| !o.is_empty()) {
        Some(o) => o.to_string(),
        None => match (env_owner.as_deref(), caller_owner) {
            (Some(env), Some(pane)) if is_project_owner(pane) && !is_project_owner(env) => {
                pane.to_string()
            }
            (Some(env), _) => env.to_string(),
            (None, Some(pane)) => pane.to_string(),
            (None, None) => "cos".to_string(),
        },
    };

    let workspace = caller
        .and_then(|p| p.workspace_id.as_deref())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let label = requested_tab.filter(|t| !t.is_empty())?;
            let mut ws: Vec<&str> = snap
                .tabs
                .iter()
                .filter(|t| t.label.as_deref() == Some(label))
                .filter_map(|t| t.workspace_id.as_deref())
                .filter(|w| !w.is_empty())
                .collect();
            ws.sort_unstable();
            ws.dedup();
            if ws.len() == 1 {
                Some(ws[0].to_string())
            } else {
                None
            }
        });

    GridScope { owner, workspace }
}

pub fn run_reconcile<D: HerdrDriver + 'static>(
    driver: &mut D,
    opts: &ReconcileOptions,
    default_tab: &str,
    deferred_statuses: Option<&[&str]>,
) -> Result<(), String> {
    reconcile_core(driver, opts, default_tab, deferred_statuses).map(|_| ())
}

/// True only for the driver that applies to a running herdr daemon from this
/// process (`CliHerdrDriver`). That driver races the plugin hook over the same
/// tabs, so it takes the shared apply lock; an in-process driver (every test
/// double) cannot race another process and must not touch the live state dir.
fn driver_is_live<D: 'static>() -> bool {
    std::any::TypeId::of::<D>() == std::any::TypeId::of::<CliHerdrDriver>()
}

pub fn reconcile_core<D: HerdrDriver + 'static>(
    driver: &mut D,
    opts: &ReconcileOptions,
    default_tab: &str,
    deferred_statuses: Option<&[&str]>,
) -> Result<bool, String> {
    let tab = opts.tab.as_deref().unwrap_or(default_tab);
    let requested = opts.tab.clone().filter(|t| !t.is_empty());

    if opts.check {
        let timeout_sec = opts.timeout_sec.unwrap_or_else(|| {
            std::env::var("GRID_CHECK_TIMEOUT_SEC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60)
        });

        let start_ts = now_sec();
        let poll = poll_interval();
        let mut lock_noted = false;

        loop {
            // Finding 7: Retry snapshot failure until timeout
            let snap = match driver.snapshot() {
                Ok(s) => s,
                Err(e) => {
                    let elapsed = now_sec().saturating_sub(start_ts);
                    if elapsed >= timeout_sec {
                        return Err(format!(
                            "grid check snapshot failed after {timeout_sec}s: {e}"
                        ));
                    }
                    sleep(poll);
                    continue;
                }
            };

            let scope = grid_scope(&snap, opts.owner.as_deref(), Some(tab));
            let ops = plan_with_deferred(&snap, deferred_statuses)?;
            let rem_ops: Vec<Op> = filter_ops_for_check(
                &ops,
                &snap,
                Some(&scope.owner),
                opts.only.as_deref(),
                Some(tab),
                scope.workspace.as_deref(),
            )
            .into_iter()
            .cloned()
            .collect();
            // The same scope without the `--only` name filter: a scoped
            // worker's move can need a runnable prerequisite from an idle
            // sibling in the same tab (pB lands only after pA moves right of
            // p9). Applying those stays inside owner/tab/workspace scope.
            let scoped_ops: Vec<Op> = filter_ops_for_check(
                &ops,
                &snap,
                Some(&scope.owner),
                None,
                Some(tab),
                scope.workspace.as_deref(),
            )
            .into_iter()
            .cloned()
            .collect();

            if opts.dry_run {
                let serialized = serde_json::to_string(&rem_ops).map_err(|e| e.to_string())?;
                println!("{serialized}");
                if rem_ops.is_empty() {
                    return Ok(false);
                } else {
                    return Err(format!("grid check pending ops: {}", rem_ops.len()));
                }
            }

            if rem_ops.is_empty() {
                return Ok(false);
            }

            // A rebuild op whose shape needs a deferred (`working`) sibling
            // cannot reach its cell: applying it just repeats the same op on
            // every poll and times the check out while `worker prompt` waits.
            // Report it deferred and converge; a later pass heals it once the
            // sibling idles.
            let runnable: Vec<Op> = scoped_ops
                .iter()
                .filter(|op| !blocked_by_deferred_rebuild(&snap, &ops, op))
                .cloned()
                .collect();
            if runnable.is_empty() {
                for op in scoped_ops
                    .iter()
                    .filter(|op| blocked_by_deferred_rebuild(&snap, &ops, op))
                {
                    eprintln!("deferred: {} (waiting for a working sibling)", op.pane);
                }
                return Ok(false);
            }

            let elapsed = now_sec().saturating_sub(start_ts);
            if elapsed >= timeout_sec {
                let serialized = serde_json::to_string(&rem_ops).unwrap_or_default();
                return Err(format!(
                    "grid check timed out after {timeout_sec}s; pending ops: {serialized}"
                ));
            }

            // Heal in place: a move deferred while its pane was `working` turns
            // runnable when the pane idles during the poll loop, so the check
            // applies the runnable ops with the same apply path the repair uses
            // instead of only re-planning until the deadline. Deferred ops never
            // reach `rem_ops` (a pane still `working` stays deferred), and
            // `apply_ops` re-checks every pane against a fresh snapshot before
            // moving it. An apply error is a pane that went busy between the
            // plan and the move; the next poll re-plans it as deferred, while a
            // persistent one leaves the ops pending for the timeout report.
            //
            // The live driver shares the tabs with the plugin hook. The hook
            // holds the shared apply lock while it parks/splits; a poll that
            // finds it held skips its apply and retries on the next poll (the
            // deadline still bounds the wait) instead of blocking `worker
            // prompt` on another process.
            let apply_result = if driver_is_live::<D>() {
                match super::lock::try_acquire() {
                    Ok(Some(_guard)) => Some(apply_ops(driver, &runnable, Some(&snap))),
                    Ok(None) => {
                        if !lock_noted {
                            eprintln!(
                                "grid check: apply skipped, the hook holds the grid apply lock; retrying"
                            );
                            lock_noted = true;
                        }
                        None
                    }
                    Err(e) => {
                        if !lock_noted {
                            eprintln!("grid check: apply lock unavailable: {e}");
                            lock_noted = true;
                        }
                        None
                    }
                }
            } else {
                Some(apply_ops(driver, &runnable, Some(&snap)))
            };
            if let Some(Err(e)) = apply_result {
                eprintln!("grid check: apply skipped: {e}");
            }

            sleep(poll);
        }
    }

    // Repair / apply mode. The live driver shares the daemon with the plugin
    // hook, whose live mode applies ops on every status event. Both park panes
    // in a scratch tab before an in-tab split, so the apply itself takes the
    // same lock the hook holds; a hook apply is short, the lock waits ~5s.
    let snap = driver.snapshot()?;
    let scope = grid_scope(&snap, opts.owner.as_deref(), Some(tab));

    let only_set: Option<HashSet<&str>> = opts.only.as_deref().map(|s| {
        s.split(',')
            .map(|x| x.trim())
            .filter(|x| !x.is_empty())
            .collect()
    });

    let def_list = deferred_statuses.unwrap_or(&["working"]);
    let is_deferred = |st: Option<&str>| st.map(|s| def_list.contains(&s)).unwrap_or(false);

    let owner_workers: Vec<&super::types::Pane> = snap
        .panes
        .iter()
        .filter(|p| {
            if !is_candidate_worker(p) {
                return false;
            }
            let powner = p
                .tokens
                .as_ref()
                .and_then(|t| t.grid_owner.as_deref())
                .unwrap_or("cos");
            if !scope.owner.is_empty() && powner != scope.owner {
                return false;
            }
            // The invoker's workspace only: repairing another project's panes
            // from here is what made a Lead's check move the CoS's panes.
            match (scope.workspace.as_deref(), p.workspace_id.as_deref()) {
                (Some(ws), Some(pw)) if !ws.is_empty() => pw == ws,
                _ => true,
            }
        })
        .collect();

    let total_workers = owner_workers.len();
    if total_workers == 0 {
        eprintln!("no workers to arrange");
        return Ok(false);
    }

    let owner_ws = match scope.workspace.as_deref() {
        Some(ws) if !ws.is_empty() => ws,
        _ => owner_workers[0].workspace_id.as_deref().unwrap_or(""),
    };
    let anchor_pane_id =
        super::plan::find_owner_pane(&scope.owner, &snap.panes, &snap.agents, owner_ws);
    let anchor_pane = anchor_pane_id
        .as_ref()
        .and_then(|id| snap.panes.iter().find(|p| &p.pane_id == id));
    let anchor_tab = anchor_pane.and_then(|p| p.tab_id.as_deref());

    // Distinct groups in first-seen order
    let mut groups: Vec<String> = Vec::new();
    for w in &owner_workers {
        if let Some(t) = &w.tokens {
            if let Some(g) = &t.grid_group {
                if !groups.contains(g) {
                    groups.push(g.clone());
                }
            }
        }
    }
    if groups.is_empty() {
        groups.push(tab.to_string());
    }

    let mut repair_ops: Vec<Op> = Vec::new();
    let mut movable_workers: Vec<(&super::types::Pane, String)> = Vec::new();

    for group in &groups {
        let mut gws: Vec<&super::types::Pane> = owner_workers
            .iter()
            .copied()
            .filter(|w| {
                w.tokens.as_ref().and_then(|t| t.grid_group.as_deref()) == Some(group.as_str())
            })
            .collect();

        gws.sort_by(|a, b| {
            let seq_a = super::plan::pane_seq(&a.pane_id);
            let seq_b = super::plan::pane_seq(&b.pane_id);
            seq_a.cmp(&seq_b).then_with(|| a.pane_id.cmp(&b.pane_id))
        });

        let n = gws.len();
        if n == 0 {
            continue;
        }
        if n > 8 {
            return Err(format!("too many workers in group {group}: {n} (max 8)"));
        }

        // Determine movable workers under --only for this group
        for w in &gws {
            let name = snap
                .agents
                .iter()
                .find(|a| a.pane_id.as_deref() == Some(&w.pane_id))
                .and_then(|a| a.name.as_deref())
                .or_else(|| w.tokens.as_ref().and_then(|t| t.grid_name.as_deref()))
                .or(w.label.as_deref())
                .unwrap_or("");
            if let Some(set) = &only_set {
                if !set.is_empty() && !set.contains(name) {
                    continue;
                }
            }
            movable_workers.push((w, name.to_string()));
        }

        let beside = total_workers == 1 && n == 1 && anchor_pane.is_some();
        if beside {
            let w = gws[0];
            let is_movable = movable_workers.iter().any(|(p, _)| p.pane_id == w.pane_id);
            if is_movable {
                let atab = anchor_tab.unwrap_or("");
                let in_place = w.tab_id.as_deref() == Some(atab);
                if !in_place {
                    repair_ops.push(Op {
                        op: "move".to_string(),
                        pane: w.pane_id.clone(),
                        deferred: is_deferred(w.agent_status.as_deref()),
                        target: Target::Split {
                            tab: atab.to_string(),
                            anchor: anchor_pane.unwrap().pane_id.clone(),
                            direction: "right".to_string(),
                            ratio: 0.4,
                        },
                    });
                }
            }
        } else {
            // !beside -> Dedicated workers tab (N1: never orchestrator tab!)
            // The one acceptance predicate shared with the planner and the check:
            // the group tab, or the exact label the panes were spawned into.
            // `--tab`/GRID_TAB wins when given; else fall back to the pane's
            // `grid_tab` token, so a repair without `--tab` still reuses the tab
            // the panes were spawned into instead of creating a second one.
            let expected_tab = requested.clone().or_else(|| group_expected_tab(&gws));
            let base_tab = expected_tab.clone().unwrap_or_else(|| group.clone());

            let chunks: Vec<&[&super::types::Pane]> = if n <= 4 {
                vec![&gws[..]]
            } else {
                vec![&gws[..4], &gws[4..]]
            };

            for (chunk_idx, chunk_gws) in chunks.into_iter().enumerate() {
                let m = chunk_gws.len();
                if m == 0 {
                    continue;
                }

                let chunk_label = super::plan::chunk_tab_label(&base_tab, chunk_idx);
                let chunk_ref = super::plan::chunk_tab_ref(chunk_idx);

                // Cell order == placement order (issue #79): the i-th pane placed
                // lands at cell i, so a new pane's split is fixed by the current
                // pane count alone.
                let build_orders: Vec<usize> = (0..m).collect();
                // Pivot: the tab is created by an idle worker, never by a working one.
                // A deferred anchor must not cancel the runnable panes of its own group
                // (a working pane is never moved, but the idle ones still are; the
                // working pane is reported deferred and a later pass moves it).
                let pivot_idx = chunk_gws
                    .iter()
                    .position(|w| !is_deferred(w.agent_status.as_deref()))
                    .unwrap_or(0);
                let mut order = build_orders.clone();
                if let Some(p) = order.iter().position(|i| *i == pivot_idx) {
                    order.remove(p);
                }
                order.insert(0, pivot_idx);

                // A deferred (working) pane must never drain its group: it can neither
                // create the tab nor anchor an idle pane's split (propagate_deferrals
                // would cancel that split too).
                let deferred_ids: HashSet<&str> = chunk_gws
                    .iter()
                    .filter(|w| is_deferred(w.agent_status.as_deref()))
                    .map(|w| w.pane_id.as_str())
                    .collect();

                let mut matching_tabs: Vec<&super::types::Tab> = snap
                    .tabs
                    .iter()
                    .filter(|t| {
                        let ws_ok =
                            t.workspace_id.as_deref() == Some(owner_ws) || owner_ws.is_empty();
                        let anchor_ok = anchor_tab.map(|at| t.tab_id != at).unwrap_or(true);
                        let group_ok = super::plan::accepts_chunk_tab(
                            group,
                            expected_tab.as_deref(),
                            chunk_idx,
                            t.label.as_deref(),
                        );
                        if !(ws_ok && anchor_ok && group_ok) {
                            return false;
                        }
                        let occ: Vec<&super::types::Pane> = snap
                            .panes
                            .iter()
                            .filter(|p| p.tab_id.as_deref() == Some(&t.tab_id))
                            .collect();
                        if occ.is_empty() {
                            return false;
                        }
                        occ.iter()
                            .all(|p| gws.iter().any(|gw| gw.pane_id == p.pane_id))
                    })
                    .collect();

                matching_tabs.sort_by_key(|t| {
                    let l = t.label.as_deref().unwrap_or("");
                    let exact = l == chunk_label;
                    if exact {
                        0
                    } else {
                        1
                    }
                });

                let valid_tab = matching_tabs.first().copied();

                let mut present: Vec<String> = if let Some(vt) = valid_tab {
                    snap.panes
                        .iter()
                        .filter(|p| p.tab_id.as_deref() == Some(&vt.tab_id))
                        .map(|p| p.pane_id.clone())
                        .collect()
                } else {
                    Vec::new()
                };

                let target_tab_ref = valid_tab
                    .map(|t| t.tab_id.clone())
                    .unwrap_or_else(|| chunk_ref.clone());

                // The tab holds every pane of the chunk but the cells are not
                // the target shape (a cleanup left the mirror column, or a pane
                // was removed from the wrong column): rebuild in cell order.
                let shape_wrong = match valid_tab {
                    Some(vt) if present.len() == m => {
                        let layout = snap.layouts.iter().find(|l| l.tab_id == vt.tab_id);
                        let area = layout.and_then(|l| l.area).unwrap_or(super::types::Rect {
                            x: 0,
                            y: 0,
                            width: 200,
                            height: 50,
                        });
                        let rects: Vec<Option<super::types::Rect>> = chunk_gws
                            .iter()
                            .map(|w| {
                                layout
                                    .and_then(|l| l.panes.iter().find(|p| p.pane_id == w.pane_id))
                                    .and_then(|p| p.rect)
                            })
                            .collect();
                        super::plan::layout_wrong(m, &area, &rects)
                    }
                    _ => false,
                };

                // A rebuild keeps the base pane (cell 0) where it is and
                // re-places the rest around it, so it must follow cell order.
                // The idle-first pivot is only for choosing who creates a tab
                // that does not exist yet.
                let order = if shape_wrong {
                    build_orders.clone()
                } else {
                    order
                };

                for (pos, idx) in order.iter().copied().enumerate() {
                    let w = chunk_gws[idx];
                    let is_movable = movable_workers.iter().any(|(p, _)| p.pane_id == w.pane_id);

                    if !is_movable {
                        if valid_tab
                            .map(|vt| w.tab_id.as_deref() == Some(&vt.tab_id))
                            .unwrap_or(false)
                            && !present.contains(&w.pane_id)
                        {
                            present.push(w.pane_id.clone());
                        }
                        continue;
                    }

                    // The first pane is the rebuild's base: it stays put.
                    if shape_wrong && pos == 0 {
                        continue;
                    }

                    if shape_wrong {
                        // Cell order: the base stays, every other pane splits
                        // onto its cell parent — even when that parent is
                        // `working`, because every pane of the rebuild is
                        // already in the tab. A deferred parent propagates to
                        // its children in `propagate_deferrals` below.
                        let (parent, dir) =
                            super::plan::placement(pos).expect("pos >= 1 has a parent cell");
                        repair_ops.push(Op {
                            op: "move".to_string(),
                            pane: w.pane_id.clone(),
                            deferred: is_deferred(w.agent_status.as_deref()),
                            target: Target::Split {
                                tab: target_tab_ref.clone(),
                                anchor: chunk_gws[order[parent]].pane_id.clone(),
                                direction: dir.to_string(),
                                ratio: 0.5,
                            },
                        });
                        present.push(w.pane_id.clone());
                        continue;
                    }

                    let in_place = !shape_wrong
                        && valid_tab
                            .map(|vt| w.tab_id.as_deref() == Some(&vt.tab_id))
                            .unwrap_or(false)
                        && (present.contains(&w.pane_id) || present.len() == m);

                    if in_place {
                        if !present.contains(&w.pane_id) {
                            present.push(w.pane_id.clone());
                        }
                        continue;
                    }

                    let (mode, anchor_idx, dir, ratio) = if pos == 0 {
                        ("new-tab", 0, "right", 0.5)
                    } else {
                        let (parent, dir) =
                            super::plan::placement(pos).expect("pos >= 1 has a parent cell");
                        ("split", order[parent], dir, 0.5)
                    };

                    if mode == "new-tab" {
                        if present.is_empty() && valid_tab.is_none() {
                            repair_ops.push(Op {
                                op: "move".to_string(),
                                pane: w.pane_id.clone(),
                                deferred: is_deferred(w.agent_status.as_deref()),
                                target: Target::NewTab {
                                    label: chunk_label.clone(),
                                },
                            });
                        } else {
                            let anchor_target = present
                                .iter()
                                .find(|p| *p != &w.pane_id && !deferred_ids.contains(p.as_str()))
                                .or_else(|| present.iter().find(|p| *p != &w.pane_id))
                                .cloned();
                            if let Some(anchor) = anchor_target {
                                repair_ops.push(Op {
                                    op: "move".to_string(),
                                    pane: w.pane_id.clone(),
                                    deferred: is_deferred(w.agent_status.as_deref()),
                                    target: Target::Split {
                                        tab: target_tab_ref.clone(),
                                        anchor,
                                        direction: "right".to_string(),
                                        ratio: 0.5,
                                    },
                                });
                            } else {
                                repair_ops.push(Op {
                                    op: "move".to_string(),
                                    pane: w.pane_id.clone(),
                                    deferred: is_deferred(w.agent_status.as_deref()),
                                    target: Target::NewTab {
                                        label: chunk_label.clone(),
                                    },
                                });
                            }
                        }
                    } else {
                        let want_anchor = &chunk_gws[anchor_idx].pane_id;
                        let idle_in_tab = present
                            .iter()
                            .find(|p| *p != &w.pane_id && !deferred_ids.contains(p.as_str()))
                            .cloned();
                        let any_in_tab = present.iter().find(|p| *p != &w.pane_id).cloned();
                        // Never anchor on a deferred (working) pane: it is not in
                        // the tab yet, and propagate_deferrals would cancel the
                        // idle pane's split too (n>=3, pivot != gws[0]).
                        if present.contains(want_anchor)
                            && want_anchor != &w.pane_id
                            && !deferred_ids.contains(want_anchor.as_str())
                        {
                            repair_ops.push(Op {
                                op: "move".to_string(),
                                pane: w.pane_id.clone(),
                                deferred: is_deferred(w.agent_status.as_deref()),
                                target: Target::Split {
                                    tab: target_tab_ref.clone(),
                                    anchor: want_anchor.clone(),
                                    direction: dir.to_string(),
                                    ratio,
                                },
                            });
                        } else if let Some(anchor) = idle_in_tab {
                            repair_ops.push(Op {
                                op: "move".to_string(),
                                pane: w.pane_id.clone(),
                                deferred: is_deferred(w.agent_status.as_deref()),
                                target: Target::Split {
                                    tab: target_tab_ref.clone(),
                                    anchor,
                                    direction: dir.to_string(),
                                    ratio,
                                },
                            });
                        } else if let Some(anchor) = any_in_tab {
                            repair_ops.push(Op {
                                op: "move".to_string(),
                                pane: w.pane_id.clone(),
                                deferred: is_deferred(w.agent_status.as_deref()),
                                target: Target::Split {
                                    tab: target_tab_ref.clone(),
                                    anchor,
                                    direction: dir.to_string(),
                                    ratio,
                                },
                            });
                        } else {
                            repair_ops.push(Op {
                                op: "move".to_string(),
                                pane: w.pane_id.clone(),
                                deferred: is_deferred(w.agent_status.as_deref()),
                                target: Target::NewTab {
                                    label: chunk_label.clone(),
                                },
                            });
                        }
                    }
                    present.push(w.pane_id.clone());
                }
            }
        }
    }

    // The planned flags must equal what apply_ops will really do: a split whose
    // anchor is deferred cannot run either, and reporting it as runnable would
    // hide the reason a repair moved fewer panes than it planned.
    super::plan::propagate_deferrals(&mut repair_ops, &snap.panes);

    if opts.dry_run {
        let serialized = serde_json::to_string(&repair_ops).map_err(|e| e.to_string())?;
        println!("{serialized}");
        return Ok(false);
    }

    let moves_applied = repair_ops.iter().any(|op| !op.deferred);
    let _lock = if moves_applied && driver_is_live::<D>() {
        Some(super::lock::acquire()?)
    } else {
        None
    };
    let pane_map = apply_ops(driver, &repair_ops, Some(&snap))?;

    // A working pane is never moved; say so instead of exiting 0 silently.
    for op in repair_ops.iter().filter(|op| op.deferred) {
        let status = snap
            .panes
            .iter()
            .find(|p| p.pane_id == op.pane)
            .and_then(|p| p.agent_status.as_deref())
            .filter(|s| def_list.contains(s))
            .unwrap_or("working");
        eprintln!("deferred: {} ({})", op.pane, status);
    }

    // Finding 1 remainder: Rename failure is NOT swallowed; matches bash set -e!
    let mut renames_applied = false;
    for (w, target_name) in movable_workers {
        let cur_pane = pane_map
            .get(&w.pane_id)
            .cloned()
            .unwrap_or_else(|| w.pane_id.clone());
        if !is_deferred(w.agent_status.as_deref())
            && !target_name.is_empty()
            && w.label.as_deref() != Some(target_name.as_str())
        {
            renames_applied = true;
        }
        driver.rename_pane(&cur_pane, &target_name)?;
    }

    let changed = moves_applied || renames_applied;
    Ok(changed)
}

/// In-process grid repair: the owner-wide arrange used by the pipeline after a
/// spawn/cleanup. `tab` is the workers tab: the scope it reuses and the label of
/// the tab it creates. Idle panes move; working ones stay put and are reported
/// `deferred: <pane> (<status>)` on stderr.
pub fn run_grid_repair(tab: &str, owner: Option<&str>) -> Result<(), String> {
    let opts = ReconcileOptions {
        check: false,
        repair: true,
        tab: Some(tab.to_string()),
        only: None,
        owner: owner.map(str::to_string),
        dry_run: false,
        timeout_sec: None,
    };
    let mut driver = CliHerdrDriver::new();
    run_reconcile(&mut driver, &opts, tab, None)
}

/// In-process grid check: the Rust replacement for the legacy arranger's
/// `--check` mode. Used by worker spawn/cleanup and by the pipeline before
/// prompting a worker; the caller owns the GRID_SKIP guard. `tab` is both the
/// requested tab and the default label.
pub fn run_grid_check(tab: &str, only: Option<&str>, owner: Option<&str>) -> Result<(), String> {
    let opts = ReconcileOptions {
        check: true,
        repair: false,
        tab: Some(tab.to_string()),
        only: only.map(str::to_string),
        owner: owner.map(str::to_string),
        dry_run: false,
        timeout_sec: None,
    };
    let mut driver = CliHerdrDriver::new();
    run_reconcile(&mut driver, &opts, tab, None)
}

fn resolve_default_tab(
    explicit_tab: Option<&str>,
    owner: Option<&str>,
    repo_name: Option<&str>,
    env_grid_tab: Option<&str>,
) -> String {
    if let Some(t) = explicit_tab {
        if !t.is_empty() {
            return t.to_string();
        }
    }
    if let Some(gt) = env_grid_tab {
        if !gt.is_empty() {
            return gt.to_string();
        }
    }
    let o = owner.unwrap_or("cos");
    let r = repo_name.unwrap_or("quaqua");
    crate::worker::spawn::default_grid_tab(o, r)
}

/// Deterministic engine activity executing agent grid operations.
pub struct GridActivity<D = CliHerdrDriver> {
    driver: Mutex<D>,
    default_tab: Option<String>,
    owner: Option<String>,
    repo: Option<String>,
    deferred_statuses: Option<Vec<String>>,
}

impl GridActivity<CliHerdrDriver> {
    pub fn new() -> Self {
        Self {
            driver: Mutex::new(CliHerdrDriver::new()),
            default_tab: None,
            owner: None,
            repo: None,
            deferred_statuses: None,
        }
    }
}

impl Default for GridActivity<CliHerdrDriver> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D: HerdrDriver> GridActivity<D> {
    pub fn with_driver(driver: D) -> Self {
        Self {
            driver: Mutex::new(driver),
            default_tab: None,
            owner: None,
            repo: None,
            deferred_statuses: None,
        }
    }

    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }

    pub fn with_default_tab(mut self, tab: impl Into<String>) -> Self {
        self.default_tab = Some(tab.into());
        self
    }

    pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    pub fn with_deferred_statuses(mut self, statuses: Vec<String>) -> Self {
        self.deferred_statuses = Some(statuses);
        self
    }
}

impl<D: HerdrDriver + Send + 'static> Activity for GridActivity<D> {
    fn execute(&self, req: &ActivityRequest) -> Result<ActivityResult, ActivityError> {
        let effective_owner = self
            .owner
            .clone()
            .or_else(|| std::env::var("QUAQUA_OWNER").ok())
            .unwrap_or_else(|| "cos".to_string());
        let effective_repo = self
            .repo
            .clone()
            .or_else(|| std::env::var("PIPELINE_REPO").ok())
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            })
            .unwrap_or_else(|| "quaqua".to_string());
        let env_grid_tab = std::env::var("GRID_TAB").ok();

        match req {
            ActivityRequest::Grid(GridRequest::Reconcile { tab, only }) => {
                let mut driver = self
                    .driver
                    .lock()
                    .map_err(|e| ActivityError::ExecutionFailed(e.to_string()))?;
                // A pipeline run from a project Lead's pane inherits the CoS's
                // `QUAQUA_OWNER`; the caller's tagged pane is the authoritative
                // owner, exactly as in `worker spawn`.
                let snap = driver.snapshot().map_err(ActivityError::ExecutionFailed)?;
                let requested_owner = self.owner.as_deref().filter(|o| is_project_owner(o));
                let target_tab = tab.as_deref().or(self.default_tab.as_deref());
                let resolved_tab = resolve_default_tab(
                    target_tab,
                    Some(&effective_owner),
                    Some(&effective_repo),
                    env_grid_tab.as_deref(),
                );
                let scope = grid_scope(&snap, requested_owner, Some(&resolved_tab));
                let opts = ReconcileOptions {
                    check: false,
                    repair: true,
                    tab: Some(resolved_tab.clone()),
                    only: only.clone(),
                    owner: Some(scope.owner),
                    dry_run: false,
                    timeout_sec: None,
                };
                let def_slices = self
                    .deferred_statuses
                    .as_ref()
                    .map(|v| v.iter().map(|s| s.as_str()).collect::<Vec<_>>());
                let changed =
                    reconcile_core(&mut *driver, &opts, &resolved_tab, def_slices.as_deref())
                        .map_err(ActivityError::ExecutionFailed)?;
                Ok(ActivityResult::Grid(GridResult::Reconciled { changed }))
            }
            ActivityRequest::Grid(GridRequest::Plan { tab }) => {
                let mut driver = self
                    .driver
                    .lock()
                    .map_err(|e| ActivityError::ExecutionFailed(e.to_string()))?;
                let target_tab = tab.as_deref().or(self.default_tab.as_deref());
                let resolved_tab = resolve_default_tab(
                    target_tab,
                    Some(&effective_owner),
                    Some(&effective_repo),
                    env_grid_tab.as_deref(),
                );
                let snap = driver.snapshot().map_err(ActivityError::ExecutionFailed)?;
                let def_slices = self
                    .deferred_statuses
                    .as_ref()
                    .map(|v| v.iter().map(|s| s.as_str()).collect::<Vec<_>>());
                let scope = grid_scope(
                    &snap,
                    self.owner.as_deref().filter(|o| is_project_owner(o)),
                    Some(&resolved_tab),
                );
                let ops = plan_with_deferred(&snap, def_slices.as_deref())
                    .map_err(ActivityError::ExecutionFailed)?;
                let filtered_ops: Vec<Op> = filter_ops_for_check(
                    &ops,
                    &snap,
                    Some(&scope.owner),
                    None,
                    Some(&resolved_tab),
                    scope.workspace.as_deref(),
                )
                .into_iter()
                .cloned()
                .collect();
                let json = serde_json::to_string(&filtered_ops)
                    .map_err(|e| ActivityError::ExecutionFailed(e.to_string()))?;
                Ok(ActivityResult::Grid(GridResult::PlanJson(json)))
            }
            other => Err(ActivityError::ExecutionFailed(format!(
                "unsupported activity request for GridActivity: {other:?}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::activity::{
        Activity, ActivityError, ActivityRequest, ActivityResult, CostRequest, GridRequest,
        GridResult,
    };
    use crate::grid::types::{
        Agent, Layout, Op, Pane, PaneLayout, Rect, Snapshot, Tab, Target, Tokens,
    };
    use crate::grid::MoveResult;

    fn make_worker(id: &str, group: &str, tab_id: Option<&str>, status: Option<&str>) -> Pane {
        Pane {
            pane_id: id.to_string(),
            tab_id: tab_id.map(str::to_string),
            workspace_id: Some("ws1".to_string()),
            agent: Some(id.to_string()),
            agent_status: status.map(str::to_string),
            tokens: Some(Tokens {
                grid_role: Some("worker".to_string()),
                grid_owner: Some("cos".to_string()),
                grid_group: Some(group.to_string()),
                grid_tab: Some(format!("{group}-workers")),
                grid_ready: Some(serde_json::json!(1)),
                grid_name: Some(id.to_string()),
            }),
            label: Some(id.to_string()),
        }
    }

    #[derive(Debug, Clone, Default)]
    struct MockHerdrDriver {
        pub snapshot: Snapshot,
        pub move_calls: Vec<(String, Target)>,
        pub rename_calls: Vec<(String, String)>,
        pub snapshot_calls: usize,
        /// `(call number, pane ids)`: those panes turn idle on that snapshot
        /// call, modelling a `working` agent that finishes during the check's
        /// poll loop.
        pub idle_on_snapshot: Option<(usize, Vec<String>)>,
    }

    impl HerdrDriver for MockHerdrDriver {
        fn snapshot(&mut self) -> Result<Snapshot, String> {
            self.snapshot_calls += 1;
            if let Some((n, panes)) = self.idle_on_snapshot.clone() {
                if n == self.snapshot_calls {
                    for p in &mut self.snapshot.panes {
                        if panes.iter().any(|id| id == &p.pane_id) {
                            p.agent_status = Some("idle".to_string());
                        }
                    }
                }
            }
            Ok(self.snapshot.clone())
        }

        fn move_pane(&mut self, pane: &str, target: &Target) -> Result<MoveResult, String> {
            self.move_calls.push((pane.to_string(), target.clone()));
            match target {
                Target::NewTab { label } => {
                    let tid = format!("tab:{label}");
                    if let Some(p) = self.snapshot.panes.iter_mut().find(|p| p.pane_id == pane) {
                        p.tab_id = Some(tid.clone());
                    }
                    if !self.snapshot.tabs.iter().any(|t| t.tab_id == tid) {
                        self.snapshot.tabs.push(Tab {
                            tab_id: tid.clone(),
                            workspace_id: Some("ws1".to_string()),
                            label: Some(label.clone()),
                            pane_count: Some(1),
                        });
                    }
                    Ok(MoveResult {
                        pane_id: pane.to_string(),
                        tab_id: tid,
                        previous_pane_id: None,
                    })
                }
                Target::Split { tab, .. } => {
                    let resolved_tab = if tab == "@workers" {
                        "tab:grp-workers".to_string()
                    } else if tab == "@workers-2" {
                        "tab:grp-workers-2".to_string()
                    } else {
                        tab.clone()
                    };
                    if let Some(p) = self.snapshot.panes.iter_mut().find(|p| p.pane_id == pane) {
                        p.tab_id = Some(resolved_tab.clone());
                    }
                    // A real herdr layout is stale after a move; drop it so a
                    // re-plan takes the unknown-shape path instead of rebuilding
                    // the same cells forever.
                    self.snapshot.layouts.retain(|l| l.tab_id != resolved_tab);
                    Ok(MoveResult {
                        pane_id: pane.to_string(),
                        tab_id: resolved_tab,
                        previous_pane_id: None,
                    })
                }
            }
        }

        fn rename_pane(&mut self, pane: &str, label: &str) -> Result<(), String> {
            self.rename_calls
                .push((pane.to_string(), label.to_string()));
            if let Some(p) = self.snapshot.panes.iter_mut().find(|p| p.pane_id == pane) {
                p.label = Some(label.to_string());
            }
            Ok(())
        }
    }

    #[test]
    fn test_plan_arrangements_1_to_4_workers() {
        // 1 worker: full / single window placement in target tab
        let snap1 = Snapshot {
            panes: vec![make_worker("w1:t1:p1", "grp", None, None)],
            ..Default::default()
        };
        let ops1 = plan_with_deferred(&snap1, None).unwrap();
        assert_eq!(ops1.len(), 1);
        assert_eq!(
            ops1[0].target,
            Target::NewTab {
                label: "grp-workers".to_string()
            }
        );

        // 2 workers: left | right (horizontal 50% split)
        let snap2 = Snapshot {
            panes: vec![
                make_worker("w1:t1:p1", "grp", None, None),
                make_worker("w1:t1:p2", "grp", None, None),
            ],
            ..Default::default()
        };
        let ops2 = plan_with_deferred(&snap2, None).unwrap();
        assert_eq!(ops2.len(), 2);
        assert_eq!(
            ops2[0].target,
            Target::NewTab {
                label: "grp-workers".to_string()
            }
        );
        assert_eq!(
            ops2[1].target,
            Target::Split {
                tab: "@workers".to_string(),
                anchor: "w1:t1:p1".to_string(),
                direction: "right".to_string(),
                ratio: 0.5,
            }
        );

        // 3 workers: A │ B/C (left full, right stacked) — issue #79
        let snap3 = Snapshot {
            panes: vec![
                make_worker("w1:t1:p1", "grp", None, None),
                make_worker("w1:t1:p2", "grp", None, None),
                make_worker("w1:t1:p3", "grp", None, None),
            ],
            ..Default::default()
        };
        let ops3 = plan_with_deferred(&snap3, None).unwrap();
        assert_eq!(ops3.len(), 3);
        assert_eq!(ops3[1].pane, "w1:t1:p2");
        assert_eq!(
            ops3[1].target,
            Target::Split {
                tab: "@workers".to_string(),
                anchor: "w1:t1:p1".to_string(),
                direction: "right".to_string(),
                ratio: 0.5,
            }
        );
        assert_eq!(ops3[2].pane, "w1:t1:p3");
        assert_eq!(
            ops3[2].target,
            Target::Split {
                tab: "@workers".to_string(),
                anchor: "w1:t1:p2".to_string(),
                direction: "down".to_string(),
                ratio: 0.5,
            }
        );

        // 4 workers: 2x2 grid A B / D C (the 4th splits the left column)
        let snap4 = Snapshot {
            panes: vec![
                make_worker("w1:t1:p1", "grp", None, None),
                make_worker("w1:t1:p2", "grp", None, None),
                make_worker("w1:t1:p3", "grp", None, None),
                make_worker("w1:t1:p4", "grp", None, None),
            ],
            ..Default::default()
        };
        let ops4 = plan_with_deferred(&snap4, None).unwrap();
        assert_eq!(ops4.len(), 4);
        assert_eq!(ops4[1].pane, "w1:t1:p2");
        assert_eq!(
            ops4[1].target,
            Target::Split {
                tab: "@workers".to_string(),
                anchor: "w1:t1:p1".to_string(),
                direction: "right".to_string(),
                ratio: 0.5,
            }
        );
        assert_eq!(ops4[2].pane, "w1:t1:p3");
        assert_eq!(
            ops4[2].target,
            Target::Split {
                tab: "@workers".to_string(),
                anchor: "w1:t1:p2".to_string(),
                direction: "down".to_string(),
                ratio: 0.5,
            }
        );
        assert_eq!(ops4[3].pane, "w1:t1:p4");
        assert_eq!(
            ops4[3].target,
            Target::Split {
                tab: "@workers".to_string(),
                anchor: "w1:t1:p1".to_string(),
                direction: "down".to_string(),
                ratio: 0.5,
            }
        );
    }

    #[test]
    fn test_plan_overflow_tab_5_to_8_workers() {
        let snap = Snapshot {
            panes: vec![
                make_worker("w1:t1:p1", "grp", None, None),
                make_worker("w1:t1:p2", "grp", None, None),
                make_worker("w1:t1:p3", "grp", None, None),
                make_worker("w1:t1:p4", "grp", None, None),
                make_worker("w1:t1:p5", "grp", None, None),
                make_worker("w1:t1:p6", "grp", None, None),
            ],
            ..Default::default()
        };
        let ops = plan_with_deferred(&snap, None).unwrap();
        assert_eq!(ops.len(), 6);

        // Chunk 0: workers 1-4 targeted to @workers (grp-workers)
        assert_eq!(
            ops[0].target,
            Target::NewTab {
                label: "grp-workers".to_string()
            }
        );
        for op in &ops[1..4] {
            if let Target::Split { tab, .. } = &op.target {
                assert_eq!(tab, "@workers");
            } else {
                panic!("expected split target for chunk 0");
            }
        }

        // Chunk 1: workers 5-6 targeted to @workers-2 (grp-workers-2)
        assert_eq!(
            ops[4].target,
            Target::NewTab {
                label: "grp-workers-2".to_string()
            }
        );
        assert_eq!(
            ops[5].target,
            Target::Split {
                tab: "@workers-2".to_string(),
                anchor: "w1:t1:p5".to_string(),
                direction: "right".to_string(),
                ratio: 0.5,
            }
        );
    }

    #[test]
    fn test_plan_deferred_working_panes() {
        let snap = Snapshot {
            panes: vec![
                make_worker("w1:t1:p1", "grp", None, Some("working")),
                make_worker("w1:t1:p2", "grp", None, Some("idle")),
            ],
            ..Default::default()
        };
        let mut ops = plan_with_deferred(&snap, None).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(ops[0].deferred, "working pane must be marked deferred");

        // Propagating deferrals marks dependent child splits deferred too
        super::super::plan::propagate_deferrals(&mut ops, &snap.panes);
        assert!(
            ops[1].deferred,
            "split anchored on deferred pane must also be deferred"
        );
    }

    fn panes_in_tab(driver: &MockHerdrDriver, label: &str) -> Vec<String> {
        let tab_id = match driver
            .snapshot
            .tabs
            .iter()
            .find(|t| t.label.as_deref() == Some(label))
        {
            Some(t) => t.tab_id.clone(),
            None => return Vec::new(),
        };
        driver
            .snapshot
            .panes
            .iter()
            .filter(|p| p.tab_id.as_deref() == Some(tab_id.as_str()))
            .map(|p| p.pane_id.clone())
            .collect()
    }

    fn check_opts(tab: &str, timeout_sec: u64) -> ReconcileOptions {
        ReconcileOptions {
            check: true,
            repair: false,
            tab: Some(tab.to_string()),
            only: None,
            owner: Some("cos".to_string()),
            dry_run: false,
            timeout_sec: Some(timeout_sec),
        }
    }

    #[test]
    fn test_check_applies_deferred_move_once_its_pane_idles() {
        // Four spawned panes are still stacked in the orchestrator tab; p2 was
        // `working` when the spawn's repair ran, so its move was deferred. The
        // check sees it working on its first snapshot and idle on the next one:
        // it must apply the runnable ops and then the deferred moves instead of
        // polling until the deadline.
        let snapshot = Snapshot {
            tabs: vec![Tab {
                tab_id: "tab:cos".to_string(),
                workspace_id: Some("ws1".to_string()),
                label: Some("1".to_string()),
                pane_count: Some(4),
            }],
            panes: vec![
                make_worker("w1:t1:p1", "grp", Some("tab:cos"), Some("idle")),
                make_worker("w1:t1:p2", "grp", Some("tab:cos"), Some("working")),
                make_worker("w1:t1:p3", "grp", Some("tab:cos"), Some("idle")),
                make_worker("w1:t1:p4", "grp", Some("tab:cos"), Some("idle")),
            ],
            ..Default::default()
        };
        let mut driver = MockHerdrDriver {
            snapshot,
            idle_on_snapshot: Some((2, vec!["w1:t1:p2".to_string()])),
            ..Default::default()
        };

        run_reconcile(
            &mut driver,
            &check_opts("grp-workers", 2),
            "grp-workers",
            None,
        )
        .expect("check must heal the deferred moves once the pane idles");

        let moved: Vec<&str> = driver.move_calls.iter().map(|(p, _)| p.as_str()).collect();
        assert!(
            moved.contains(&"w1:t1:p2"),
            "the pane that was working on the first snapshot must be moved once idle: {:?}",
            driver.move_calls
        );
        assert!(
            moved.contains(&"w1:t1:p3"),
            "the split that depended on the deferred pane must follow it: {:?}",
            driver.move_calls
        );
        let in_tab = panes_in_tab(&driver, "grp-workers");
        assert_eq!(
            in_tab.len(),
            4,
            "all four workers must land in the workers tab: {in_tab:?}"
        );
    }

    #[test]
    fn test_check_never_moves_a_still_working_pane() {
        // The check heals the idle pane's move but must leave the `working` one
        // (and any split anchored on it) deferred.
        let snapshot = Snapshot {
            tabs: vec![Tab {
                tab_id: "tab:cos".to_string(),
                workspace_id: Some("ws1".to_string()),
                label: Some("1".to_string()),
                pane_count: Some(2),
            }],
            panes: vec![
                make_worker("w1:t1:p1", "grp", Some("tab:cos"), Some("idle")),
                make_worker("w1:t1:p2", "grp", Some("tab:cos"), Some("working")),
            ],
            ..Default::default()
        };
        let mut driver = MockHerdrDriver {
            snapshot,
            ..Default::default()
        };

        run_reconcile(
            &mut driver,
            &check_opts("grp-workers", 2),
            "grp-workers",
            None,
        )
        .expect("check must converge after applying the runnable pane");

        assert!(
            !driver.move_calls.iter().any(|(p, _)| p == "w1:t1:p2"),
            "a pane still working must never be moved: {:?}",
            driver.move_calls
        );
        assert!(
            !driver.move_calls.iter().any(|(_, t)| matches!(
                t,
                Target::Split { anchor, .. } if anchor == "w1:t1:p2"
            )),
            "no split may be anchored on a pane still working: {:?}",
            driver.move_calls
        );
        let p2 = driver
            .snapshot
            .panes
            .iter()
            .find(|p| p.pane_id == "w1:t1:p2")
            .unwrap();
        assert_eq!(
            p2.tab_id.as_deref(),
            Some("tab:cos"),
            "the working pane must stay in its tab"
        );
    }

    /// Live repro (2026-10-22): four workers already live in `quaqua-workers`,
    /// stacked vertically (the wrong shape); p6/pB are idle, p9/pA are
    /// `working`. The 2x2 rebuild asks pB to split below p6, but that op can
    /// only land once p9 moves right of p6 — p9 is deferred, so the op cannot
    /// converge and the check used to time out (blocking `worker prompt`).
    fn stacked_workers_snapshot() -> Snapshot {
        let rect = |x, y, width, height| {
            Some(Rect {
                x,
                y,
                width,
                height,
            })
        };
        Snapshot {
            tabs: vec![Tab {
                tab_id: "w1:t3".to_string(),
                workspace_id: Some("ws1".to_string()),
                label: Some("quaqua-workers".to_string()),
                pane_count: Some(4),
            }],
            layouts: vec![Layout {
                tab_id: "w1:t3".to_string(),
                workspace_id: Some("ws1".to_string()),
                area: Some(Rect {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 64,
                }),
                panes: vec![
                    PaneLayout {
                        pane_id: "w1:t3:p6".to_string(),
                        rect: rect(0, 0, 200, 8),
                    },
                    PaneLayout {
                        pane_id: "w1:t3:pB".to_string(),
                        rect: rect(0, 8, 200, 8),
                    },
                    PaneLayout {
                        pane_id: "w1:t3:pA".to_string(),
                        rect: rect(0, 16, 200, 16),
                    },
                    PaneLayout {
                        pane_id: "w1:t3:p9".to_string(),
                        rect: rect(0, 32, 200, 32),
                    },
                ],
                zoomed: Some(false),
            }],
            panes: vec![
                make_worker("w1:t3:p6", "quaqua", Some("w1:t3"), Some("idle")),
                make_worker("w1:t3:p9", "quaqua", Some("w1:t3"), Some("working")),
                make_worker("w1:t3:pA", "quaqua", Some("w1:t3"), Some("working")),
                make_worker("w1:t3:pB", "quaqua", Some("w1:t3"), Some("idle")),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn test_check_defers_rebuild_that_waits_on_working_siblings() {
        let mut driver = MockHerdrDriver {
            snapshot: stacked_workers_snapshot(),
            ..Default::default()
        };
        let opts = super::ReconcileOptions {
            check: true,
            repair: false,
            tab: Some("quaqua-workers".to_string()),
            only: Some("w1:t3:pB".to_string()),
            owner: Some("cos".to_string()),
            dry_run: false,
            timeout_sec: Some(2),
        };

        run_reconcile(&mut driver, &opts, "quaqua-workers", None)
            .expect("a rebuild that waits on working siblings must not fail the check");

        assert!(
            !driver.move_calls.iter().any(|(p, _)| p == "w1:t3:pB"),
            "the blocked rebuild op must stay deferred instead of being applied every poll: {:?}",
            driver.move_calls
        );
    }

    /// Third live repro (2026-10-22): three workers in `quaqua-workers`,
    /// stacked vertically (the wrong shape); p9 is `working`, pA and pB are
    /// idle. The `A │ B/C` target (p9 left full, pA top-right, pB
    /// bottom-right) can be built by moving pA and pB only.
    fn stacked_three_workers_snapshot() -> Snapshot {
        let rect = |x, y, width, height| {
            Some(Rect {
                x,
                y,
                width,
                height,
            })
        };
        Snapshot {
            tabs: vec![Tab {
                tab_id: "w1:t3".to_string(),
                workspace_id: Some("ws1".to_string()),
                label: Some("quaqua-workers".to_string()),
                pane_count: Some(3),
            }],
            layouts: vec![Layout {
                tab_id: "w1:t3".to_string(),
                workspace_id: Some("ws1".to_string()),
                area: Some(Rect {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 64,
                }),
                panes: vec![
                    PaneLayout {
                        pane_id: "w1:t3:p9".to_string(),
                        rect: rect(0, 0, 200, 21),
                    },
                    PaneLayout {
                        pane_id: "w1:t3:pA".to_string(),
                        rect: rect(0, 21, 200, 21),
                    },
                    PaneLayout {
                        pane_id: "w1:t3:pB".to_string(),
                        rect: rect(0, 42, 200, 22),
                    },
                ],
                zoomed: Some(false),
            }],
            panes: vec![
                make_worker("w1:t3:p9", "quaqua", Some("w1:t3"), Some("working")),
                make_worker("w1:t3:pA", "quaqua", Some("w1:t3"), Some("idle")),
                make_worker("w1:t3:pB", "quaqua", Some("w1:t3"), Some("idle")),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn test_repair_rebuilds_around_a_working_base_pane() {
        let mut driver = MockHerdrDriver {
            snapshot: stacked_three_workers_snapshot(),
            ..Default::default()
        };
        let opts = super::ReconcileOptions {
            check: false,
            repair: true,
            tab: Some("quaqua-workers".to_string()),
            only: None,
            owner: Some("cos".to_string()),
            dry_run: false,
            timeout_sec: None,
        };

        run_reconcile(&mut driver, &opts, "quaqua-workers", None)
            .expect("repair must build around a working base pane");

        let splits: Vec<(&str, &Target)> = driver
            .move_calls
            .iter()
            .filter(|(_, t)| matches!(t, Target::Split { .. }))
            .map(|(p, t)| (p.as_str(), t))
            .collect();
        assert_eq!(
            splits.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            vec!["w1:t3:pA", "w1:t3:pB"],
            "only the idle panes may move, in cell order: {:?}",
            driver.move_calls
        );
        assert_eq!(
            splits[0].1,
            &Target::Split {
                tab: "w1:t3".to_string(),
                anchor: "w1:t3:p9".to_string(),
                direction: "right".to_string(),
                ratio: 0.5,
            }
        );
        assert_eq!(
            splits[1].1,
            &Target::Split {
                tab: "w1:t3".to_string(),
                anchor: "w1:t3:pA".to_string(),
                direction: "down".to_string(),
                ratio: 0.5,
            }
        );
        assert!(
            !driver.move_calls.iter().any(|(p, _)| p == "w1:t3:p9"),
            "the working base pane must never be moved: {:?}",
            driver.move_calls
        );
    }

    #[test]
    fn test_check_only_applies_prerequisites_of_the_scoped_worker() {
        // `worker prompt pB` scopes the check with --only pB; pB can only land
        // after pA moves right of p9, so the check must also apply pA's
        // runnable move (same owner, tab and workspace) instead of waiting.
        let mut driver = MockHerdrDriver {
            snapshot: stacked_three_workers_snapshot(),
            ..Default::default()
        };
        let opts = super::ReconcileOptions {
            check: true,
            repair: false,
            tab: Some("quaqua-workers".to_string()),
            only: Some("w1:t3:pB".to_string()),
            owner: Some("cos".to_string()),
            dry_run: false,
            timeout_sec: Some(2),
        };

        run_reconcile(&mut driver, &opts, "quaqua-workers", None)
            .expect("check must apply the scoped worker's runnable prerequisites");

        let splits: Vec<&str> = driver
            .move_calls
            .iter()
            .filter(|(_, t)| matches!(t, Target::Split { .. }))
            .map(|(p, _)| p.as_str())
            .collect();
        assert_eq!(
            splits,
            vec!["w1:t3:pA", "w1:t3:pB"],
            "the prerequisite pA must move before the scoped pB: {:?}",
            driver.move_calls
        );
        assert!(
            !driver.move_calls.iter().any(|(p, _)| p == "w1:t3:p9"),
            "the working pane must never be moved: {:?}",
            driver.move_calls
        );
    }

    /// Six workers of one group: p1-p4 idly stacked (wrong shape) in the base
    /// tab, p5/p6 not placed yet — the overflow chunk.
    fn base_tab_with_overflow_snapshot() -> Snapshot {
        let rect = |x, y, width, height| {
            Some(Rect {
                x,
                y,
                width,
                height,
            })
        };
        Snapshot {
            tabs: vec![Tab {
                tab_id: "w1:t3".to_string(),
                workspace_id: Some("ws1".to_string()),
                label: Some("quaqua-workers".to_string()),
                pane_count: Some(4),
            }],
            layouts: vec![Layout {
                tab_id: "w1:t3".to_string(),
                workspace_id: Some("ws1".to_string()),
                area: Some(Rect {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 100,
                }),
                panes: vec![
                    PaneLayout {
                        pane_id: "w1:t3:p1".to_string(),
                        rect: rect(0, 0, 200, 25),
                    },
                    PaneLayout {
                        pane_id: "w1:t3:p2".to_string(),
                        rect: rect(0, 25, 200, 25),
                    },
                    PaneLayout {
                        pane_id: "w1:t3:p3".to_string(),
                        rect: rect(0, 50, 200, 25),
                    },
                    PaneLayout {
                        pane_id: "w1:t3:p4".to_string(),
                        rect: rect(0, 75, 200, 25),
                    },
                ],
                zoomed: Some(false),
            }],
            panes: vec![
                make_worker("w1:t3:p1", "quaqua", Some("w1:t3"), Some("idle")),
                make_worker("w1:t3:p2", "quaqua", Some("w1:t3"), Some("idle")),
                make_worker("w1:t3:p3", "quaqua", Some("w1:t3"), Some("idle")),
                make_worker("w1:t3:p4", "quaqua", Some("w1:t3"), Some("idle")),
                make_worker("w1:t3:p5", "quaqua", None, Some("idle")),
                make_worker("w1:t3:p6", "quaqua", None, Some("idle")),
            ],
            ..Default::default()
        }
    }

    fn is_overflow_target(target: &Target) -> bool {
        match target {
            Target::NewTab { label } => label.ends_with("-workers-2"),
            Target::Split { tab, .. } => tab == "@workers-2" || tab.ends_with("-workers-2"),
        }
    }

    #[test]
    fn test_check_scoped_to_the_base_tab_never_applies_overflow_ops() {
        // `grid reconcile --check --tab quaqua-workers --only p2` heals p2's
        // runnable prerequisites inside the base tab. p5/p6 belong to
        // `quaqua-workers-2`; the old filter dropped chunk-1 ops only when the
        // *requested* tab was the overflow one, so the scoped ops moved p5/p6
        // into the overflow tab while checking the base tab.
        let mut driver = MockHerdrDriver {
            snapshot: base_tab_with_overflow_snapshot(),
            ..Default::default()
        };
        let opts = super::ReconcileOptions {
            check: true,
            repair: false,
            tab: Some("quaqua-workers".to_string()),
            only: Some("w1:t3:p2".to_string()),
            owner: Some("cos".to_string()),
            dry_run: false,
            timeout_sec: Some(2),
        };

        let res = run_reconcile(&mut driver, &opts, "quaqua-workers", None);

        let overflow: Vec<&(String, Target)> = driver
            .move_calls
            .iter()
            .filter(|(_, t)| is_overflow_target(t))
            .collect();
        assert!(
            overflow.is_empty(),
            "a check scoped to quaqua-workers must not apply overflow ops: {:?}",
            driver.move_calls
        );
        assert!(
            driver.move_calls.iter().any(|(p, _)| p == "w1:t3:p2"),
            "the scoped worker's prerequisite must still move: {:?}",
            driver.move_calls
        );
        res.expect("the check must converge inside the base tab");
    }

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            EnvGuard { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn lockcase_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "quaqua-grid-lockcase-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Two idle workers stacked in the wrong shape: one runnable split.
    fn two_stacked_workers_snapshot() -> Snapshot {
        Snapshot {
            tabs: vec![Tab {
                tab_id: "w1:t1".to_string(),
                workspace_id: Some("ws1".to_string()),
                label: Some("quaqua-workers".to_string()),
                pane_count: Some(2),
            }],
            layouts: vec![Layout {
                tab_id: "w1:t1".to_string(),
                workspace_id: Some("ws1".to_string()),
                area: Some(Rect {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 100,
                }),
                panes: vec![
                    PaneLayout {
                        pane_id: "w1:t1:p1".to_string(),
                        rect: Some(Rect {
                            x: 0,
                            y: 0,
                            width: 200,
                            height: 50,
                        }),
                    },
                    PaneLayout {
                        pane_id: "w1:t1:p2".to_string(),
                        rect: Some(Rect {
                            x: 0,
                            y: 50,
                            width: 200,
                            height: 50,
                        }),
                    },
                ],
                zoomed: Some(false),
            }],
            panes: vec![
                make_worker("w1:t1:p1", "quaqua", Some("w1:t1"), Some("idle")),
                make_worker("w1:t1:p2", "quaqua", Some("w1:t1"), Some("idle")),
            ],
            ..Default::default()
        }
    }

    /// Stub `herdr` for the live driver type: answers `api snapshot` from
    /// `snap` and records every `pane move` into `calls` (one line per move).
    fn stub_herdr(
        dir: &std::path::Path,
        snap: &Snapshot,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let snap_file = dir.join("snapshot.json");
        let calls_file = dir.join("moves");
        std::fs::write(&snap_file, serde_json::to_string(snap).unwrap()).unwrap();
        let stub = dir.join("herdr");
        let script = format!(
            r#"#!/bin/sh
if [ "$1 $2" = "api snapshot" ]; then
  cat "{snap}"
  exit 0
fi
if [ "$1 $2" = "pane move" ]; then
  printf '%s\n' "$3" >> "{calls}"
  printf '{{"result":{{"move_result":{{"changed":true,"reason":"ok","pane":{{"pane_id":"%s","tab_id":"tab:moved"}}}}}}}}\n' "$3"
  exit 0
fi
exit 0
"#,
            snap = snap_file.display(),
            calls = calls_file.display()
        );
        std::fs::write(&stub, script).unwrap();
        let mut perms = std::fs::metadata(&stub).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).unwrap();
        (stub, calls_file)
    }

    /// Another process takes the hook's lock directory and holds it: a live
    /// `sleep` owns the pid in the lock file, exactly like a running hook.
    fn hold_lock_in_child(state_dir: &std::path::Path, hold_secs: u64) -> std::process::Child {
        let sleep_bin = if std::path::Path::new("/bin/sleep").is_file() {
            "/bin/sleep"
        } else if std::path::Path::new("/usr/bin/sleep").is_file() {
            "/usr/bin/sleep"
        } else {
            "sleep"
        };
        let child = std::process::Command::new(sleep_bin)
            .arg(hold_secs.to_string())
            .spawn()
            .expect("spawn lock holder");
        std::fs::create_dir_all(state_dir.join("lock")).expect("create lock dir");
        std::fs::write(state_dir.join("lock/pid"), child.id().to_string())
            .expect("write holder pid");
        child
    }

    fn recorded_moves(calls: &std::path::Path) -> String {
        std::fs::read_to_string(calls).unwrap_or_default()
    }

    #[test]
    fn test_check_does_not_apply_while_the_hook_holds_the_lock() {
        let _serial = crate::grid::lock::test_serial();
        let dir = lockcase_dir("check-held");
        let state = dir.join("state");
        let (stub, calls) = stub_herdr(&dir, &two_stacked_workers_snapshot());
        let _env = EnvGuard::set("HERDR_PLUGIN_STATE_DIR", state.to_str().unwrap());

        let mut holder = hold_lock_in_child(&state, 2);

        let mut driver = crate::herdr::Herdr {
            bin: stub.to_string_lossy().to_string(),
        };
        let opts = super::ReconcileOptions {
            check: true,
            repair: false,
            tab: Some("quaqua-workers".to_string()),
            only: None,
            owner: Some("cos".to_string()),
            dry_run: false,
            timeout_sec: Some(1),
        };
        let res = run_reconcile(&mut driver, &opts, "quaqua-workers", None);

        assert!(
            recorded_moves(&calls).is_empty(),
            "the check applied ops while another process held the lock: {}",
            recorded_moves(&calls)
        );
        assert!(
            res.is_err(),
            "a check that could not apply must report the pending op, not converge"
        );

        holder.wait().expect("lock holder exits");
        let opts = super::ReconcileOptions {
            timeout_sec: Some(3),
            ..opts
        };
        // The second pass applies; it may still time out on the stub's static
        // layout, so only the applied move is asserted.
        let _ = run_reconcile(&mut driver, &opts, "quaqua-workers", None);
        assert!(
            !recorded_moves(&calls).is_empty(),
            "the check must apply once the lock is free (res={res:?}, calls={:?})",
            recorded_moves(&calls)
        );
    }

    #[test]
    fn test_repair_waits_for_the_hook_lock_instead_of_racing() {
        let _serial = crate::grid::lock::test_serial();
        let dir = lockcase_dir("repair-held");
        let state = dir.join("state");
        let (stub, calls) = stub_herdr(&dir, &two_stacked_workers_snapshot());
        let _env = EnvGuard::set("HERDR_PLUGIN_STATE_DIR", state.to_str().unwrap());

        let mut holder = hold_lock_in_child(&state, 1);

        let bin = stub.to_string_lossy().to_string();
        let calls_for_thread = calls.clone();
        let repair = std::thread::spawn(move || {
            let mut driver = crate::herdr::Herdr { bin };
            let opts = super::ReconcileOptions {
                check: false,
                repair: true,
                tab: Some("quaqua-workers".to_string()),
                only: None,
                owner: Some("cos".to_string()),
                dry_run: false,
                timeout_sec: None,
            };
            run_reconcile(&mut driver, &opts, "quaqua-workers", None)
        });

        std::thread::sleep(Duration::from_millis(300));
        assert!(
            recorded_moves(&calls_for_thread).is_empty(),
            "the repair applied ops while another process held the lock: {}",
            recorded_moves(&calls_for_thread)
        );

        holder.wait().expect("lock holder exits");
        repair
            .join()
            .expect("repair thread")
            .expect("the repair must wait for the lock, then apply");
        assert!(
            !recorded_moves(&calls).is_empty(),
            "the repair must apply after the holder releases the lock"
        );
    }

    #[test]
    fn test_repair_rebuilds_the_mirror_column_after_cleanup() {
        // Issue #79 cleanup, 4->3 removing a right-column pane: the tab is left
        // with the left column stacked and the right column full (the mirror).
        // The repair must re-place the panes instead of reporting "in place".
        let area = crate::grid::types::Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 100,
        };
        let rect = |x, y, width, height| {
            Some(crate::grid::types::Rect {
                x,
                y,
                width,
                height,
            })
        };
        let mut panes = vec![Pane {
            pane_id: "w1:p1".to_string(),
            tab_id: Some("w1:t1".to_string()),
            workspace_id: Some("ws1".to_string()),
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
        }];
        panes.extend([
            make_worker("w1:p2", "grp", Some("tab:grp-workers"), None), // A
            make_worker("w1:p4", "grp", Some("tab:grp-workers"), None), // C
            make_worker("w1:p5", "grp", Some("tab:grp-workers"), None), // D
        ]);
        let snapshot = Snapshot {
            tabs: vec![
                Tab {
                    tab_id: "w1:t1".to_string(),
                    workspace_id: Some("ws1".to_string()),
                    label: Some("1".to_string()),
                    pane_count: Some(1),
                },
                Tab {
                    tab_id: "tab:grp-workers".to_string(),
                    workspace_id: Some("ws1".to_string()),
                    label: Some("grp-workers".to_string()),
                    pane_count: Some(3),
                },
            ],
            layouts: vec![super::super::types::Layout {
                tab_id: "tab:grp-workers".to_string(),
                workspace_id: Some("ws1".to_string()),
                area: Some(area),
                panes: vec![
                    super::super::types::PaneLayout {
                        pane_id: "w1:p2".to_string(),
                        rect: rect(0, 0, 100, 50),
                    },
                    super::super::types::PaneLayout {
                        pane_id: "w1:p4".to_string(),
                        rect: rect(0, 50, 100, 50),
                    },
                    super::super::types::PaneLayout {
                        pane_id: "w1:p5".to_string(),
                        rect: rect(100, 0, 100, 100),
                    },
                ],
                zoomed: Some(false),
            }],
            panes,
            ..Default::default()
        };
        assert!(
            crate::grid::plan::layout_wrong(
                3,
                &area,
                &[
                    Some(crate::grid::types::Rect {
                        x: 0,
                        y: 0,
                        width: 100,
                        height: 50
                    }),
                    Some(crate::grid::types::Rect {
                        x: 0,
                        y: 50,
                        width: 100,
                        height: 50
                    }),
                    Some(crate::grid::types::Rect {
                        x: 100,
                        y: 0,
                        width: 100,
                        height: 100
                    }),
                ]
            ),
            "this layout must read as the mirror"
        );

        let mut driver = MockHerdrDriver {
            snapshot,
            ..Default::default()
        };
        run_reconcile(
            &mut driver,
            &super::ReconcileOptions {
                check: false,
                repair: true,
                tab: Some("grp-workers".to_string()),
                only: None,
                owner: Some("cos".to_string()),
                dry_run: false,
                timeout_sec: None,
            },
            "workers",
            None,
        )
        .expect("mirror repair must succeed");

        // apply_ops parks each pane in a scratch tab first (a same-tab split is
        // a `same_tab` no-op in herdr), then splits it onto the parent cell.
        let splits: Vec<(String, Target)> = driver
            .move_calls
            .iter()
            .filter(|(_, t)| matches!(t, Target::Split { .. }))
            .cloned()
            .collect();
        let moves: Vec<&str> = splits.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            moves,
            vec!["w1:p4", "w1:p5"],
            "rebuild in cell order: {:?}",
            driver.move_calls
        );
        assert_eq!(
            splits[0].1,
            Target::Split {
                tab: "tab:grp-workers".to_string(),
                anchor: "w1:p2".to_string(),
                direction: "right".to_string(),
                ratio: 0.5,
            }
        );
        assert_eq!(
            splits[1].1,
            Target::Split {
                tab: "tab:grp-workers".to_string(),
                anchor: "w1:p4".to_string(),
                direction: "down".to_string(),
                ratio: 0.5,
            }
        );
        assert!(
            driver.move_calls.iter().any(
                |(_, t)| matches!(t, Target::NewTab { label } if label.starts_with("grid-scratch-"))
            ),
            "each in-tab move must park first: {:?}",
            driver.move_calls
        );
    }

    #[test]
    fn test_reconcile_activity_changed_when_moves_applied() {
        let _lock = crate::test_env::lock_env();
        let _env_owner = crate::test_env::TestEnvVarGuard::remove("QUAQUA_OWNER");
        let mock = MockHerdrDriver {
            snapshot: Snapshot {
                panes: vec![
                    make_worker("w1:t1:p1", "grp", Some("tab:other"), None),
                    make_worker("w1:t1:p2", "grp", Some("tab:other"), None),
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let activity = GridActivity::with_driver(mock).with_owner("cos");
        let req = ActivityRequest::Grid(GridRequest::Reconcile {
            tab: Some("grp-workers".to_string()),
            only: None,
        });
        let res = activity.execute(&req).unwrap();
        assert_eq!(
            res,
            ActivityResult::Grid(GridResult::Reconciled { changed: true })
        );
    }

    #[test]
    fn test_reconcile_activity_unchanged_when_already_in_place() {
        let mock = MockHerdrDriver {
            snapshot: Snapshot {
                tabs: vec![Tab {
                    tab_id: "tab:grp-workers".to_string(),
                    workspace_id: Some("ws1".to_string()),
                    label: Some("grp-workers".to_string()),
                    pane_count: Some(2),
                }],
                panes: vec![
                    make_worker("w1:t1:p1", "grp", Some("tab:grp-workers"), None),
                    make_worker("w1:t1:p2", "grp", Some("tab:grp-workers"), None),
                ],
                agents: vec![
                    Agent {
                        name: Some("w1:t1:p1".to_string()),
                        pane_id: Some("w1:t1:p1".to_string()),
                        agent_status: None,
                    },
                    Agent {
                        name: Some("w1:t1:p2".to_string()),
                        pane_id: Some("w1:t1:p2".to_string()),
                        agent_status: None,
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let activity = GridActivity::with_driver(mock).with_owner("cos");
        let req = ActivityRequest::Grid(GridRequest::Reconcile {
            tab: Some("grp-workers".to_string()),
            only: None,
        });
        let res = activity.execute(&req).unwrap();
        assert_eq!(
            res,
            ActivityResult::Grid(GridResult::Reconciled { changed: false })
        );
    }

    #[test]
    fn test_reconcile_activity_unchanged_when_all_deferred() {
        let mock = MockHerdrDriver {
            snapshot: Snapshot {
                panes: vec![
                    make_worker("w1:t1:p1", "grp", Some("tab:other"), Some("working")),
                    make_worker("w1:t1:p2", "grp", Some("tab:other"), Some("working")),
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let activity = GridActivity::with_driver(mock).with_owner("cos");
        let req = ActivityRequest::Grid(GridRequest::Reconcile {
            tab: Some("grp-workers".to_string()),
            only: None,
        });
        let res = activity.execute(&req).unwrap();
        assert_eq!(
            res,
            ActivityResult::Grid(GridResult::Reconciled { changed: false })
        );
    }

    #[test]
    fn test_reconcile_activity_mixed_working_and_idle() {
        let _lock = crate::test_env::lock_env();
        let _env_owner = crate::test_env::TestEnvVarGuard::remove("QUAQUA_OWNER");
        let mock = MockHerdrDriver {
            snapshot: Snapshot {
                panes: vec![
                    make_worker("w1:t1:p1", "grp", Some("tab:other"), Some("working")),
                    make_worker("w1:t1:p2", "grp", Some("tab:other"), Some("idle")),
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let activity = GridActivity::with_driver(mock).with_owner("cos");
        let req = ActivityRequest::Grid(GridRequest::Reconcile {
            tab: Some("grp-workers".to_string()),
            only: None,
        });
        let res = activity.execute(&req).unwrap();
        assert_eq!(
            res,
            ActivityResult::Grid(GridResult::Reconciled { changed: true })
        );

        let driver = activity.driver.lock().unwrap();
        // Working pane must NEVER be moved
        assert!(
            !driver.move_calls.iter().any(|(p, _)| p == "w1:t1:p1"),
            "working pane w1:t1:p1 must not be moved"
        );
        // Idle pane must be moved
        assert!(
            driver.move_calls.iter().any(|(p, _)| p == "w1:t1:p2"),
            "idle pane w1:t1:p2 must be moved"
        );
        // No split must be anchored on the working pane
        for (_, target) in &driver.move_calls {
            if let Target::Split { anchor, .. } = target {
                assert_ne!(
                    anchor, "w1:t1:p1",
                    "split must not be anchored on working pane"
                );
            }
        }
    }

    #[test]
    fn test_resolve_default_tab_conventions() {
        // Explicit tab always wins
        assert_eq!(
            resolve_default_tab(Some("custom-tab"), None, None, None),
            "custom-tab"
        );
        assert_eq!(
            resolve_default_tab(
                Some("custom-tab"),
                Some("arch-pipe-lead"),
                Some("quaqua"),
                Some("env-tab")
            ),
            "custom-tab"
        );

        // Ambient GRID_TAB wins over owner/repo default
        assert_eq!(
            resolve_default_tab(
                None,
                Some("arch-pipe-lead"),
                Some("quaqua"),
                Some("env-tab")
            ),
            "env-tab"
        );

        // Lead owner convention: strips -lead and appends -workers
        assert_eq!(
            resolve_default_tab(None, Some("arch-pipe-lead"), Some("quaqua"), None),
            "arch-pipe-workers"
        );

        // CoS owner convention: uses repo name + -workers
        assert_eq!(
            resolve_default_tab(None, Some("cos"), Some("quaqua"), None),
            "quaqua-workers"
        );
        assert_eq!(
            resolve_default_tab(None, None, Some("monotinhtute"), None),
            "monotinhtute-workers"
        );
    }

    #[test]
    fn test_grid_activity_plan_json() {
        let _lock = crate::test_env::lock_env();
        let _env_owner = crate::test_env::TestEnvVarGuard::remove("QUAQUA_OWNER");
        let mock = MockHerdrDriver {
            snapshot: Snapshot {
                panes: vec![
                    make_worker("w1:t1:p1", "grp", None, None),
                    make_worker("w1:t1:p2", "grp", None, None),
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let activity = GridActivity::with_driver(mock)
            .with_owner("cos")
            .with_default_tab("grp-workers");
        let req = ActivityRequest::Grid(GridRequest::Plan { tab: None });
        let res = activity.execute(&req).unwrap();
        if let ActivityResult::Grid(GridResult::PlanJson(json)) = res {
            let ops: Vec<Op> = serde_json::from_str(&json).unwrap();
            assert_eq!(ops.len(), 2);
        } else {
            panic!("expected PlanJson result, got {:?}", res);
        }
    }

    #[test]
    fn test_grid_activity_rejects_unsupported_request() {
        let activity = GridActivity::new();
        let req = ActivityRequest::Cost(CostRequest::QuerySpend {
            session_pattern: "session".to_string(),
        });
        let res = activity.execute(&req);
        assert!(matches!(res, Err(ActivityError::ExecutionFailed(_))));
    }
}
