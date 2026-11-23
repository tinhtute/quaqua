//! Pane placement: four workers per tab, a fifth opens `<repo>-workers-2`.
//!
//! Placement is a pure function over the tabs' occupancy; the herdr calls it
//! implies go through [`PaneOps`]. List-and-claim runs under [`lock::GridLock`]
//! and a claim is a `grid_role=worker` token, so two placements never share a
//! pane. Reconcile and cleanup close only a pane recorded in `agent.pane_id`.

pub mod arrange;
pub mod lock;

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

/// One live pane as herdr reports it (`pane list` joined with `tab list`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pane {
    pub id: String,
    /// Label of the pane's tab, from `tab list` by `tab_id`.
    pub tab: String,
    /// The harness `pane list` reports in `agent` (e.g. `claude`), not an agent name.
    pub harness: Option<String>,
    pub role: Option<String>,
    pub owner: Option<String>,
    pub farm: Option<String>,
    pub tab_id: String,
    /// Label of the pane's workspace, from `workspace list` (its id when unlisted).
    pub workspace: String,
    pub workspace_id: String,
    /// herdr's `agent_status` (`working`, `idle`...).
    pub status: Option<String>,
    /// Token `grid_tab`: the tab the pane belongs in.
    pub grid_tab: Option<String>,
    /// Token `grid_group`: the project slug (the Lead's workspace label).
    pub group: Option<String>,
}

impl Pane {
    /// A pane that hosts a harness or was claimed for a worker.
    pub fn taken(&self) -> bool {
        self.harness.is_some() || self.role.as_deref() == Some("worker")
    }
}

/// Who a claim or a reconcile acts for. `None` fields do not narrow.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scope {
    pub owner: Option<String>,
    pub farm: Option<String>,
}

/// The farm id of a workspace: the last component of the directory holding
/// `runs/`, lowercased, outside `[a-z0-9_-]` replaced by `-`.
pub fn farm_id(runs_dir: &std::path::Path) -> Option<String> {
    let root = runs_dir
        .parent()?
        .file_name()?
        .to_string_lossy()
        .to_lowercase();
    Some(
        root.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect(),
    )
}

impl Scope {
    pub fn from_runs_dir(runs_dir: &std::path::Path) -> Scope {
        Scope {
            owner: std::env::var("QUAQUA_OWNER").ok().filter(|o| !o.is_empty()),
            farm: farm_id(runs_dir),
        }
    }

    /// Tokens only narrow: a CoS/Lead pane, or a pane of another owner or farm,
    /// is never ours. An untagged pane passes (the DB record is what claims it).
    pub fn admits(&self, p: &Pane) -> bool {
        let same = |mine: &Option<String>, theirs: &Option<String>| match (mine, theirs) {
            (Some(m), Some(t)) => m == t,
            _ => true,
        };
        matches!(p.role.as_deref(), None | Some("worker"))
            && same(&self.owner, &p.owner)
            && same(&self.farm, &p.farm)
    }
}

/// How a new pane is cut from its anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Right,
    Down,
}

impl Dir {
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::Right => "right",
            Dir::Down => "down",
        }
    }
}

/// The pane operations a placement and a release need. The real client gets
/// them from [`HerdrPanes`]; tests may implement them in memory.
pub trait PaneOps {
    fn panes(&self) -> std::io::Result<Vec<Pane>>;
    /// Opens a tab and returns its first pane.
    fn create_tab(&self, label: &str) -> std::io::Result<Pane>;
    /// Splits `anchor` (a pane id) and returns the new pane.
    fn split(&self, anchor: &str, dir: Dir) -> std::io::Result<Pane>;
    fn close_pane(&self, id: &str) -> std::io::Result<()>;
    /// Marks `pane` as a worker pane of `scope`.
    fn claim(&self, pane: &str, scope: &Scope) -> std::io::Result<()>;
    /// Moves `pane` and returns its id afterwards (herdr may change it across
    /// workspaces). Default `Unsupported`, like the herdr client's.
    fn move_pane(
        &self,
        _pane: &str,
        _to: &crate::herdr::MoveTo,
    ) -> std::io::Result<Option<String>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "pane move not supported by this client",
        ))
    }
}

/// Occupancy of `repo`'s worker tabs, from the live panes.
pub fn occupancy(repo: &str, panes: &[Pane]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while panes.iter().any(|p| p.tab == tab_label(repo, i)) {
        let label = tab_label(repo, i);
        out.push(panes.iter().filter(|p| p.tab == label && p.taken()).count());
        i += 1;
    }
    out
}

/// Where the n-th pane (0-based) of a tab is cut, as `(anchor index, dir)`:
/// 2 = right of the first, 3 = below the second (`A | B/C`), 4 = below the
/// first (`A B / D C`).
pub fn cut(n: usize) -> (usize, Dir) {
    match n {
        1 => (0, Dir::Right),
        2 => (1, Dir::Down),
        _ => (0, Dir::Down),
    }
}

/// Returns a claimed pane for the next worker of `repo`: an untaken pane of a
/// tab with room, else a cut of that tab, else a new tab. List, choice and
/// claim happen under the grid lock held in `lock_dir`.
pub fn acquire(
    ops: &dyn PaneOps,
    repo: &str,
    scope: &Scope,
    lock_dir: &std::path::Path,
) -> std::io::Result<String> {
    let _guard = lock::GridLock::take(lock_dir)?;
    let panes = ops.panes()?;
    let target = place(repo, &occupancy(repo, &panes));
    let in_tab: Vec<&Pane> = panes.iter().filter(|p| p.tab == target.tab).collect();
    let id = if target.new_tab || in_tab.is_empty() {
        ops.create_tab(&target.tab)?.id
    } else if let Some(free) = in_tab.iter().find(|p| !p.taken()) {
        free.id.clone()
    } else {
        let (anchor, dir) = cut(in_tab.len());
        let anchor = in_tab
            .get(anchor)
            .or(in_tab.last())
            .map(|p| p.id.clone())
            .unwrap_or_default();
        ops.split(&anchor, dir)?.id
    };
    ops.claim(&id, scope)?;
    Ok(id)
}

/// What `grid reconcile` does about one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// A pane recorded for an agent the database has ended.
    Close(String),
}

/// One spawn-recorded pane and whether an open agent still holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The agent name the pane was recorded for.
    pub name: String,
    pub pane: String,
    pub open: bool,
}

/// What `grid reconcile --only/--tab` narrows a reconcile to. Both only
/// narrow: an empty filter is the whole scope.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    /// Agent names (`--only a,b`): only their panes. Empty = no narrowing.
    pub only: Vec<String>,
    /// Exact tab label (`--tab X`): `X-2` is another tab and is left alone.
    pub tab: Option<String>,
}

impl Filter {
    /// `--only` is a comma-separated name set; blanks are dropped, so
    /// `--only ""` narrows nothing.
    pub fn only_from(list: &str) -> Vec<String> {
        list.split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(String::from)
            .collect()
    }

    fn admits(&self, p: &Pane, recorded: &[Recorded]) -> bool {
        self.tab.as_deref().is_none_or(|t| p.tab == t)
            && (self.only.is_empty()
                || recorded
                    .iter()
                    .any(|r| r.pane == p.id && self.only.contains(&r.name)))
    }
}

/// Panes to close: live, recorded at spawn, held by no open agent, and
/// admitted by `scope`. Everything else (a CoS, a human's shell, another
/// farm's worker) is left alone; a repeat finds nothing.
pub fn reconcile_plan(panes: &[Pane], recorded: &[Recorded], scope: &Scope) -> Vec<Op> {
    reconcile_plan_filtered(panes, recorded, scope, &Filter::default())
}

/// [`reconcile_plan`] narrowed by `filter`.
pub fn reconcile_plan_filtered(
    panes: &[Pane],
    recorded: &[Recorded],
    scope: &Scope,
    filter: &Filter,
) -> Vec<Op> {
    panes
        .iter()
        .filter(|p| {
            filter.admits(p, recorded)
                && recorded.iter().any(|r| r.pane == p.id && !r.open)
                && !recorded.iter().any(|r| r.pane == p.id && r.open)
                && scope.admits(p)
        })
        .map(|p| Op::Close(p.id.clone()))
        .collect()
}

/// The panes the database recorded at spawn.
pub fn recorded_panes(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<Recorded>> {
    let mut st = conn.prepare(
        "SELECT name, pane_id, ended_ms IS NULL FROM agent WHERE pane_id IS NOT NULL AND pane_id != ''",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Recorded {
            name: r.get(0)?,
            pane: r.get(1)?,
            open: r.get(2)?,
        })
    })?;
    rows.collect()
}

/// Applies [`reconcile_plan`] and returns the ops it ran.
pub fn reconcile(
    ops: &dyn PaneOps,
    conn: &rusqlite::Connection,
    scope: &Scope,
) -> Result<Vec<Op>, String> {
    reconcile_filtered(ops, conn, scope, &Filter::default())
}

/// [`reconcile`] narrowed by `filter`.
pub fn reconcile_filtered(
    ops: &dyn PaneOps,
    conn: &rusqlite::Connection,
    scope: &Scope,
    filter: &Filter,
) -> Result<Vec<Op>, String> {
    let panes = ops.panes().map_err(|e| e.to_string())?;
    let recorded = recorded_panes(conn).map_err(|e| e.to_string())?;
    let plan = reconcile_plan_filtered(&panes, &recorded, scope, filter);
    for op in &plan {
        match op {
            Op::Close(id) => ops.close_pane(id).map_err(|e| e.to_string())?,
        }
    }
    Ok(plan)
}

/// The move step of `grid reconcile`: [`arrange::arrange`] over the farm's
/// panes, narrowed by `filter` (`--only` names resolve through the database).
pub fn arrange_filtered(
    ops: &dyn PaneOps,
    conn: &rusqlite::Connection,
    scope: &Scope,
    filter: &Filter,
) -> Result<arrange::Arranged, String> {
    let panes = if filter.only.is_empty() {
        None
    } else {
        Some(
            recorded_panes(conn)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|r| filter.only.contains(&r.name))
                .map(|r| r.pane)
                .collect(),
        )
    };
    let only = arrange::Only {
        panes,
        tab: filter.tab.clone(),
    };
    let done = arrange::arrange(
        ops,
        scope.farm.as_deref(),
        &only,
        &arrange::deferred_statuses(conn),
    )
    .map_err(|e| e.to_string())?;
    // herdr may give a pane a new id when it crosses workspaces: keep the
    // recorded id live, or reconcile and cleanup could no longer close it.
    for (old, new) in done.moved.iter().filter(|(o, n)| o != n) {
        conn.execute(
            "UPDATE agent SET pane_id = ?2 WHERE pane_id = ?1 AND ended_ms IS NULL",
            [old, new],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(done)
}

/// [`PaneOps`] over any [`HerdrClient`]: the one adapter from herdr replies.
pub struct HerdrPanes<'a>(pub &'a dyn crate::herdr::HerdrClient);

/// The JSON of a herdr reply to `call`. An ok reply with an empty body (herdr
/// prints nothing for `pane report-metadata`) is `Null`; a failed or
/// malformed reply names the call.
fn reply_json(call: &str, r: crate::herdr::Reply) -> std::io::Result<serde_json::Value> {
    if !r.ok {
        let why = r.error_code().unwrap_or(r.body);
        return Err(std::io::Error::other(format!("herdr {call}: {why}")));
    }
    if r.body.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&r.body)
        .map_err(|e| std::io::Error::other(format!("herdr {call}: malformed reply: {e}")))
}

fn text<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str().filter(|s| !s.is_empty())
}

fn pane_from(
    p: &serde_json::Value,
    labels: &std::collections::HashMap<String, String>,
    workspaces: &std::collections::HashMap<String, String>,
) -> Option<Pane> {
    let tokens = p.get("tokens");
    let token = |k: &str| tokens.and_then(|t| text(t, k)).map(String::from);
    Some(Pane {
        id: text(p, "pane_id")?.to_string(),
        tab: text(p, "tab_id")
            .and_then(|t| labels.get(t))
            .cloned()
            .unwrap_or_default(),
        harness: text(p, "agent").map(String::from),
        role: token("grid_role"),
        owner: token("grid_owner"),
        farm: token("grid_farm"),
        tab_id: text(p, "tab_id").unwrap_or_default().to_string(),
        workspace: text(p, "workspace_id")
            .map(|w| workspaces.get(w).map_or(w, String::as_str))
            .unwrap_or_default()
            .to_string(),
        workspace_id: text(p, "workspace_id").unwrap_or_default().to_string(),
        status: text(p, "agent_status").map(String::from),
        grid_tab: token("grid_tab"),
        group: token("grid_group"),
    })
}

impl HerdrPanes<'_> {
    fn tab_labels(&self) -> std::io::Result<std::collections::HashMap<String, String>> {
        let v = reply_json("tab list", self.0.tab_list()?)?;
        let tabs = v
            .pointer("/result/tabs")
            .and_then(|x| x.as_array())
            .ok_or_else(|| std::io::Error::other("reply has no tabs"))?;
        Ok(tabs
            .iter()
            .filter_map(|t| {
                Some((
                    text(t, "tab_id")?.to_string(),
                    text(t, "label")?.to_string(),
                ))
            })
            .collect())
    }

    /// Workspace id -> label; a client without `workspace list` gives none and
    /// a pane's workspace is then named by its id.
    fn workspace_labels(&self) -> std::collections::HashMap<String, String> {
        let Ok(r) = self.0.workspace_list() else {
            return Default::default();
        };
        let Ok(v) = reply_json("workspace list", r) else {
            return Default::default();
        };
        v.pointer("/result/workspaces")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|w| {
                        Some((
                            text(w, "workspace_id")?.to_string(),
                            text(w, "label")?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn find(&self, id: &str) -> std::io::Result<Option<Pane>> {
        Ok(self.panes()?.into_iter().find(|p| p.id == id))
    }
}

impl PaneOps for HerdrPanes<'_> {
    fn panes(&self) -> std::io::Result<Vec<Pane>> {
        let labels = self.tab_labels()?;
        let workspaces = self.workspace_labels();
        let v = reply_json("pane list", self.0.pane_list()?)?;
        let list = v
            .pointer("/result/panes")
            .and_then(|x| x.as_array())
            .ok_or_else(|| std::io::Error::other("reply has no panes"))?;
        Ok(list
            .iter()
            .filter_map(|p| pane_from(p, &labels, &workspaces))
            .collect())
    }

    /// `tab create` returns the new tab; its first pane is `/result/root_pane`
    /// or `/result/pane` when the reply carries one, else the one pane
    /// `pane list` shows in that tab (the reply shape is not verified live).
    fn create_tab(&self, label: &str) -> std::io::Result<Pane> {
        let v = reply_json("tab create", self.0.tab_create(label)?)?;
        let direct = ["/result/root_pane/pane_id", "/result/pane/pane_id"]
            .iter()
            .find_map(|p| v.pointer(p)?.as_str().map(String::from));
        let id = match direct {
            Some(id) => id,
            None => {
                let tab = v
                    .pointer("/result/tab/tab_id")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| {
                        std::io::Error::other("tab create reply has no pane or tab id")
                    })?;
                let list = reply_json("pane list", self.0.pane_list()?)?;
                list.pointer("/result/panes")
                    .and_then(|x| x.as_array())
                    .and_then(|a| a.iter().find(|p| text(p, "tab_id") == Some(tab)))
                    .and_then(|p| text(p, "pane_id").map(String::from))
                    .ok_or_else(|| std::io::Error::other("new tab has no pane"))?
            }
        };
        Ok(Pane {
            id,
            tab: label.to_string(),
            ..Pane::default()
        })
    }

    fn split(&self, anchor: &str, dir: Dir) -> std::io::Result<Pane> {
        let tab = self.find(anchor)?.map(|p| p.tab).unwrap_or_default();
        let v = reply_json("pane split", self.0.pane_split(anchor, dir.as_str())?)?;
        let id = v
            .pointer("/result/pane/pane_id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| std::io::Error::other("reply has no pane_id"))?;
        Ok(Pane {
            id: id.to_string(),
            tab,
            ..Pane::default()
        })
    }

    fn close_pane(&self, id: &str) -> std::io::Result<()> {
        reply_json("pane close", self.0.pane_close(id)?).map(|_| ())
    }

    fn move_pane(&self, pane: &str, to: &crate::herdr::MoveTo) -> std::io::Result<Option<String>> {
        let v = reply_json("pane move", self.0.pane_move(pane, to)?)?;
        let res = v.pointer("/result/move_result");
        if res.and_then(|r| r.get("changed")).and_then(|c| c.as_bool()) == Some(false) {
            return Ok(None);
        }
        Ok(Some(
            res.and_then(|r| r.pointer("/pane/pane_id"))
                .and_then(|x| x.as_str())
                .unwrap_or(pane)
                .to_string(),
        ))
    }

    fn claim(&self, pane: &str, scope: &Scope) -> std::io::Result<()> {
        let mut tokens = vec![("grid_role", "worker")];
        if let Some(o) = scope.owner.as_deref() {
            tokens.push(("grid_owner", o));
        }
        if let Some(f) = scope.farm.as_deref() {
            tokens.push(("grid_farm", f));
        }
        reply_json(
            "pane report-metadata",
            self.0.pane_report_metadata(pane, "quaqua", &tokens, None)?,
        )
        .map(|_| ())
    }
}

fn grid_command(env: &crate::cli::Env, args: &[String]) -> Result<String, crate::cli::CliError> {
    use crate::cli::{rpc_call, CliError};
    let usage = || {
        CliError::new(
            1,
            "usage: grid reconcile [--only <names>] [--tab <label>] | grid acquire <repo>",
        )
    };
    match args.first().map(String::as_str) {
        Some("reconcile") => {
            let call = reconcile_args(&args[1..]).map_err(|m| match m {
                Some(m) => CliError::new(1, m),
                None => usage(),
            })?;
            Ok(rpc_call(env, "grid.reconcile_scoped", call)?.to_string())
        }
        Some("acquire") => {
            let repo = args.get(1).ok_or_else(usage)?;
            Ok(rpc_call(env, "grid.acquire", serde_json::json!({ "repo": repo }))?.to_string())
        }
        _ => Err(usage()),
    }
}

/// The `grid.reconcile_scoped` request for `reconcile`'s flags. `Err(None)` is
/// a usage error; `Err(Some(m))` a flag with a bad value.
fn reconcile_args(args: &[String]) -> Result<serde_json::Value, Option<String>> {
    let (mut only, mut tab) = (Vec::new(), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) => (f, Some(v)),
            None => (a.as_str(), None),
        };
        if flag != "--only" && flag != "--tab" {
            return Err(None);
        }
        let value = match inline {
            Some(v) => v,
            None => it.next().ok_or(None)?.as_str(),
        };
        if flag == "--only" {
            only.extend(Filter::only_from(value));
        } else if value.is_empty() {
            return Err(Some("--tab requires a non-empty label".into()));
        } else {
            tab = Some(value.to_string());
        }
    }
    Ok(serde_json::json!({ "only": only, "tab": tab }))
}

/// `grid.reconcile_scoped {only: [name], tab: label|null}`: [`reconcile_filtered`]
/// over the daemon's herdr and database; answers the panes it closed.
fn register_reconcile(d: &mut crate::daemon::Daemon) -> Result<(), String> {
    use crate::rpc::RpcError;
    let herdr = d.herdr();
    let runs = d.runs_dir().to_path_buf();
    d.register_verb(
        "grid.reconcile_scoped",
        Box::new(move |_, req| {
            let filter = Filter {
                only: req
                    .args
                    .get("only")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|n| n.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                tab: req
                    .args
                    .get("tab")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            };
            let conn = crate::db::open(&runs.join(crate::db::path::DB_FILE))
                .map_err(|e| RpcError::new("internal", e.to_string()))?;
            let ops = reconcile_filtered(
                &HerdrPanes(herdr.as_ref()),
                &conn,
                &Scope::from_runs_dir(&runs),
                &filter,
            )
            .map_err(|e| RpcError::new("refused", e))?;
            let arranged = arrange_filtered(
                &HerdrPanes(herdr.as_ref()),
                &conn,
                &Scope::from_runs_dir(&runs),
                &filter,
            )
            .map_err(|e| RpcError::new("refused", e))?;
            let closed: Vec<&str> = ops.iter().map(|Op::Close(id)| id.as_str()).collect();
            let moved: Vec<&str> = arranged.moved.iter().map(|(_, n)| n.as_str()).collect();
            Ok(serde_json::json!({
                "closed": closed.len(),
                "panes": closed,
                "moved": moved,
                "deferred": arranged.deferred,
                "unplaced": arranged.unplaced,
            }))
        }),
    )
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
        (Some("spawn"), _) => crate::task::worker_flow::spawn_cmd::lead_spawn_cli(env, &args[1..]),
        _ => Err(CliError::new(
            1,
            "usage: lead spawn <slug> [--task <kickoff>] | lead cleanup <project>",
        )),
    }
}

/// The module's seam in `cli::MODULES`: the `grid` and `lead` commands. The
/// daemon verbs they call are registered by `task::prompt::INSTALL`.
pub const INSTALL: crate::cli::Install = install;

fn install(cli: &mut crate::cli::Registry, d: &mut crate::daemon::Daemon) -> Result<(), String> {
    register_reconcile(d)?;
    cli.register(crate::cli::Command {
        name: "grid",
        help: "reconcile [--only <names>] [--tab <tab>] | acquire <repo>",
        run: grid_command,
    })?;
    cli.register(crate::cli::Command {
        name: "lead",
        help: "spawn <slug> [--task <kickoff>] | cleanup <project>",
        run: lead_command,
    })
}
