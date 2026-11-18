//! The move step: put every token-bearing pane where the layout rule says.
//!
//! A pane's tokens say where it belongs: `grid_tab` is its tab (4 per tab, a
//! fifth opens `<tab>-2`), a worker whose `grid_owner` is `<slug>-lead` lives in
//! the Lead's workspace `<slug>`, a Lead in its own. The pass moves one pane,
//! lists again, and repeats, so ids that herdr changes on a move never go
//! stale. A pane whose status is deferred (`working`) is left where it is.

use super::{cut, Dir, Pane, PaneOps, PER_TAB};
use crate::herdr::MoveTo;
use std::collections::{HashMap, HashSet};

/// Statuses a repair does not move a pane in (`[grid] deferred_statuses`).
pub const DEFAULT_DEFERRED: &[&str] = &["working"];

/// What a pass acts on. Both fields only narrow.
#[derive(Debug, Clone, Default)]
pub struct Only {
    /// Pane ids (`--only` names resolved through the database).
    pub panes: Option<Vec<String>>,
    /// A grid tab (`--tab X`): the panes whose `grid_tab` is `X` or that sit in it.
    pub tab: Option<String>,
}

/// What a pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Arranged {
    /// `(id before, id after)` of every pane moved.
    pub moved: Vec<(String, String)>,
    /// Panes that are misplaced but deferred by their status.
    pub deferred: Vec<String>,
}

/// Label of overflow tab `i` of `base`: `base`, `base-2`, `base-3`...
fn family(base: &str, i: usize) -> String {
    if i == 0 {
        base.to_string()
    } else {
        format!("{base}-{}", i + 1)
    }
}

fn in_family(tab: &str, base: &str) -> bool {
    tab == base
        || tab
            .strip_prefix(base)
            .and_then(|r| r.strip_prefix('-'))
            .and_then(|n| n.parse::<usize>().ok())
            .is_some_and(|n| n >= 2)
}

fn role_of(p: &Pane) -> Option<&str> {
    p.role.as_deref().filter(|r| *r == "worker" || *r == "lead")
}

/// The workspace a managed pane belongs in, when its tokens say so.
fn workspace_for(p: &Pane, panes: &[Pane]) -> Option<String> {
    let lead_of = |group: &str| format!("{group}-lead");
    match role_of(p)? {
        "lead" => p.group.clone(),
        _ => {
            let owner = p.owner.as_deref()?;
            panes
                .iter()
                .filter(|l| l.role.as_deref() == Some("lead"))
                .filter_map(|l| l.group.as_deref())
                .find(|g| lead_of(g) == owner)
                .map(String::from)
        }
    }
}

enum Step {
    Move(String, MoveTo),
    Done,
}

struct Pass<'a> {
    farm: Option<&'a str>,
    only: &'a Only,
    deferred: &'a [String],
}

impl Pass<'_> {
    fn selected(&self, p: &Pane) -> bool {
        let Some(tab) = p.grid_tab.as_deref() else {
            return false;
        };
        role_of(p).is_some()
            && match (self.farm, p.farm.as_deref()) {
                (Some(m), Some(t)) => m == t,
                _ => true,
            }
            && self
                .only
                .tab
                .as_deref()
                .is_none_or(|t| t == tab || t == p.tab)
            && self
                .only
                .panes
                .as_ref()
                .is_none_or(|ids| ids.contains(&p.id))
    }

    fn is_deferred(&self, p: &Pane) -> bool {
        p.status
            .as_deref()
            .is_some_and(|s| self.deferred.iter().any(|d| d == s))
    }

    fn misplaced(&self, p: &Pane, panes: &[Pane]) -> bool {
        let base = p.grid_tab.as_deref().unwrap_or_default();
        let ws_ok = workspace_for(p, panes).is_none_or(|w| w == p.workspace);
        !(ws_ok && in_family(&p.tab, base))
    }

    /// The first move still due, Leads before their Peers; `skip` holds panes
    /// herdr would not move. Deferred panes are collected in `deferred`.
    fn next(&self, panes: &[Pane], skip: &HashSet<String>, deferred: &mut Vec<String>) -> Step {
        let mut due: Vec<&Pane> = panes
            .iter()
            .filter(|p| self.selected(p) && self.misplaced(p, panes))
            .collect();
        due.sort_by_key(|p| role_of(p) != Some("lead"));
        for p in due {
            if self.is_deferred(p) {
                deferred.push(p.id.clone());
                continue;
            }
            if skip.contains(&p.id) {
                continue;
            }
            return Step::Move(p.id.clone(), self.destination(p, panes));
        }
        Step::Done
    }

    fn destination(&self, p: &Pane, panes: &[Pane]) -> MoveTo {
        let base = p.grid_tab.clone().unwrap_or_default();
        let ws = workspace_for(p, panes).unwrap_or_else(|| p.workspace.clone());
        let Some(ws_id) = panes
            .iter()
            .find(|q| q.workspace == ws)
            .map(|q| q.workspace_id.clone())
        else {
            return MoveTo::NewWorkspace {
                label: ws,
                tab_label: base,
            };
        };
        for i in 0.. {
            let label = family(&base, i);
            let members: Vec<&Pane> = panes
                .iter()
                .filter(|q| q.workspace_id == ws_id && q.tab == label && q.id != p.id)
                .collect();
            if members.is_empty() {
                return MoveTo::NewTab {
                    workspace: Some(ws_id),
                    label,
                };
            }
            if members.len() < PER_TAB {
                let (anchor, dir) = cut(members.len());
                let anchor = members.get(anchor).unwrap_or(&members[0]);
                return MoveTo::Split {
                    tab: anchor.tab_id.clone(),
                    anchor: anchor.id.clone(),
                    dir: match dir {
                        Dir::Right => "right",
                        Dir::Down => "down",
                    }
                    .to_string(),
                };
            }
        }
        unreachable!("the family of tabs is unbounded")
    }
}

/// Moves the selected panes into the layout their tokens ask for. `farm`
/// narrows to one farm's panes; a pane whose status is in `deferred` stays.
pub fn arrange(
    ops: &dyn PaneOps,
    farm: Option<&str>,
    only: &Only,
    deferred: &[String],
) -> std::io::Result<Arranged> {
    let mut only = only.clone();
    let mut out = Arranged::default();
    let mut tries: HashMap<String, u32> = HashMap::new();
    let mut skip = HashSet::new();
    let mut panes = ops.panes()?;
    let budget = panes.len() * 3 + 8;
    for _ in 0..budget {
        let pass = Pass {
            farm,
            only: &only,
            deferred,
        };
        let mut waiting = Vec::new();
        let Step::Move(id, to) = pass.next(&panes, &skip, &mut waiting) else {
            out.deferred = waiting;
            return Ok(out);
        };
        let now = ops.move_pane(&id, &to)?;
        if let Some(ids) = only.panes.as_mut() {
            for i in ids.iter_mut().filter(|i| **i == id) {
                *i = now.clone();
            }
        }
        let n = tries.entry(now.clone()).or_default();
        *n += 1;
        if *n >= 2 {
            skip.insert(now.clone());
        }
        out.moved.push((id, now));
        panes = ops.panes()?;
    }
    let mut waiting = Vec::new();
    let pass = Pass {
        farm,
        only: &only,
        deferred,
    };
    let _ = pass.next(&panes, &skip, &mut waiting);
    out.deferred = waiting;
    Ok(out)
}

/// `[grid] deferred_statuses` from the workspace settings, `working` by default.
pub fn deferred_statuses(conn: &rusqlite::Connection) -> Vec<String> {
    crate::config::setting(conn, "grid.deferred_statuses")
        .ok()
        .flatten()
        .and_then(|v| {
            v.as_array().map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
        })
        .unwrap_or_else(|| DEFAULT_DEFERRED.iter().map(|s| s.to_string()).collect())
}
