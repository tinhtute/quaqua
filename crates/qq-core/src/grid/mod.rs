//! Pane placement: four workers per tab, a fifth opens `<repo>-workers-2`.
//!
//! Placement is a pure function over the tabs' occupancy; the herdr calls it
//! implies go through [`PaneOps`], the seam the frozen `HerdrClient` has no
//! methods for yet (`pane split|close|list`, `tab create`).

/// Workers a tab holds before the next one opens a new tab.
pub const PER_TAB: usize = 4;

/// Label of tab `index` (0-based) of `repo`: `<repo>-workers`, then `-workers-2`...
pub fn tab_label(repo: &str, index: usize) -> String {
    if index == 0 {
        format!("{repo}-workers")
    } else {
        format!("{repo}-workers-{}", index + 1)
    }
}

/// Where the next worker goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub tab_index: usize,
    pub tab: String,
    /// The tab does not exist yet and is created first.
    pub new_tab: bool,
}

/// `occupancy[i]` = workers in tab `i`. The first tab with room wins (so a
/// hole left by a cleanup is refilled); none → a new tab after the last.
pub fn place(repo: &str, occupancy: &[usize]) -> Target {
    match occupancy.iter().position(|&n| n < PER_TAB) {
        Some(i) => Target {
            tab_index: i,
            tab: tab_label(repo, i),
            new_tab: false,
        },
        None => Target {
            tab_index: occupancy.len(),
            tab: tab_label(repo, occupancy.len()),
            new_tab: true,
        },
    }
}

/// One live pane as herdr reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub id: String,
    pub tab: String,
    /// The agent running in it, when any.
    pub agent: Option<String>,
}

/// The pane operations a placement and a release need. The real client gets
/// them once the herdr seam is extended; tests implement them in memory.
pub trait PaneOps {
    fn panes(&self) -> std::io::Result<Vec<Pane>>;
    /// Opens a tab and returns its first pane.
    fn create_tab(&self, label: &str) -> std::io::Result<Pane>;
    /// Splits `anchor` (a pane id) and returns the new pane.
    fn split(&self, anchor: &str) -> std::io::Result<Pane>;
    fn close_pane(&self, id: &str) -> std::io::Result<()>;
}

/// Occupancy of `repo`'s worker tabs, from the live panes (a pane counts when
/// an agent runs in it).
pub fn occupancy(repo: &str, panes: &[Pane]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while panes.iter().any(|p| p.tab == tab_label(repo, i)) {
        let label = tab_label(repo, i);
        out.push(
            panes
                .iter()
                .filter(|p| p.tab == label && p.agent.is_some())
                .count(),
        );
        i += 1;
    }
    out
}

/// Returns a pane for the next worker of `repo`: an empty pane of a tab with
/// room, else a split of that tab, else a new tab.
pub fn acquire(ops: &dyn PaneOps, repo: &str) -> std::io::Result<String> {
    let panes = ops.panes()?;
    let target = place(repo, &occupancy(repo, &panes));
    if target.new_tab {
        return Ok(ops.create_tab(&target.tab)?.id);
    }
    let in_tab: Vec<&Pane> = panes.iter().filter(|p| p.tab == target.tab).collect();
    if let Some(free) = in_tab.iter().find(|p| p.agent.is_none()) {
        return Ok(free.id.clone());
    }
    match in_tab.last() {
        Some(anchor) => Ok(ops.split(&anchor.id)?.id),
        None => Ok(ops.create_tab(&target.tab)?.id),
    }
}

/// What `grid reconcile` does about one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// A pane whose agent the database has ended.
    Close(String),
}

/// Panes still running an agent that the DB says ended (`live` = names of
/// open agents). A pane with no agent is left alone; a repeat finds nothing.
pub fn reconcile_plan(panes: &[Pane], live: &[String]) -> Vec<Op> {
    panes
        .iter()
        .filter_map(|p| match &p.agent {
            Some(a) if !live.contains(a) => Some(Op::Close(p.id.clone())),
            _ => None,
        })
        .collect()
}

/// Names of the agents the database holds open.
pub fn live_agents(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<String>> {
    let mut st = conn.prepare("SELECT name FROM agent WHERE ended_ms IS NULL")?;
    let rows = st.query_map([], |r| r.get(0))?;
    rows.collect()
}

/// Applies [`reconcile_plan`] and returns the ops it ran.
pub fn reconcile(ops: &dyn PaneOps, conn: &rusqlite::Connection) -> Result<Vec<Op>, String> {
    let panes = ops.panes().map_err(|e| e.to_string())?;
    let live = live_agents(conn).map_err(|e| e.to_string())?;
    let plan = reconcile_plan(&panes, &live);
    for op in &plan {
        match op {
            Op::Close(id) => ops.close_pane(id).map_err(|e| e.to_string())?,
        }
    }
    Ok(plan)
}

/// [`PaneOps`] over any [`HerdrClient`]: the one adapter from herdr replies.
pub struct HerdrPanes<'a>(pub &'a dyn crate::herdr::HerdrClient);

fn reply_json(r: crate::herdr::Reply) -> std::io::Result<serde_json::Value> {
    if !r.ok {
        return Err(std::io::Error::other(r.error_code().unwrap_or(r.body)));
    }
    serde_json::from_str(&r.body).map_err(std::io::Error::other)
}

fn pane_of(v: &serde_json::Value, ptr: &str, tab: &str) -> std::io::Result<Pane> {
    let id = v
        .pointer(&format!("{ptr}/pane_id"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| std::io::Error::other("reply has no pane_id"))?;
    Ok(Pane {
        id: id.to_string(),
        tab: tab.to_string(),
        agent: None,
    })
}

impl PaneOps for HerdrPanes<'_> {
    fn panes(&self) -> std::io::Result<Vec<Pane>> {
        let v = reply_json(self.0.pane_list()?)?;
        let list = v
            .pointer("/result/panes")
            .and_then(|x| x.as_array())
            .ok_or_else(|| std::io::Error::other("reply has no panes"))?;
        Ok(list
            .iter()
            .filter_map(|p| {
                Some(Pane {
                    id: p.get("pane_id")?.as_str()?.to_string(),
                    tab: p.get("tab_label")?.as_str()?.to_string(),
                    agent: p.get("agent").and_then(|a| a.as_str()).map(String::from),
                })
            })
            .collect())
    }

    fn create_tab(&self, label: &str) -> std::io::Result<Pane> {
        let v = reply_json(self.0.tab_create(label)?)?;
        pane_of(&v, "/result/root_pane", label)
    }

    fn split(&self, anchor: &str) -> std::io::Result<Pane> {
        let tab = self
            .panes()?
            .into_iter()
            .find(|p| p.id == anchor)
            .map(|p| p.tab)
            .unwrap_or_default();
        let v = reply_json(self.0.pane_split(anchor, "right")?)?;
        pane_of(&v, "/result/pane", &tab)
    }

    fn close_pane(&self, id: &str) -> std::io::Result<()> {
        reply_json(self.0.pane_close(id)?).map(|_| ())
    }
}

fn grid_command(env: &crate::cli::Env, args: &[String]) -> Result<String, crate::cli::CliError> {
    use crate::cli::{rpc_call, CliError};
    let usage = || CliError::new(1, "usage: grid reconcile | grid acquire <repo>");
    match args.first().map(String::as_str) {
        Some("reconcile") => {
            Ok(rpc_call(env, "grid.reconcile", serde_json::json!({}))?.to_string())
        }
        Some("acquire") => {
            let repo = args.get(1).ok_or_else(usage)?;
            Ok(rpc_call(env, "grid.acquire", serde_json::json!({ "repo": repo }))?.to_string())
        }
        _ => Err(usage()),
    }
}

fn lead_command(env: &crate::cli::Env, args: &[String]) -> Result<String, crate::cli::CliError> {
    use crate::cli::{rpc_call, CliError};
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("cleanup"), Some(project)) => Ok(rpc_call(
            env,
            "lead.cleanup",
            serde_json::json!({ "project": project }),
        )?
        .to_string()),
        _ => Err(CliError::new(1, "usage: lead cleanup <project>")),
    }
}

/// The module's seam in `cli::MODULES`: the `grid` and `lead` commands. The
/// daemon verbs they call are registered by `task::prompt::INSTALL`.
pub const INSTALL: crate::cli::Install = install;

fn install(cli: &mut crate::cli::Registry, _: &mut crate::daemon::Daemon) -> Result<(), String> {
    cli.register(crate::cli::Command {
        name: "grid",
        help: "reconcile | acquire <repo>",
        run: grid_command,
    })?;
    cli.register(crate::cli::Command {
        name: "lead",
        help: "cleanup <project>",
        run: lead_command,
    })
}
