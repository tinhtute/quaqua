//! `status`: the farm view, read from the database only.

use rusqlite::{params, Connection};

#[derive(Debug, Clone, PartialEq)]
pub struct AgentRow {
    pub name: String,
    pub role: String,
    pub kind: Option<String>,
    pub project: Option<String>,
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub workflow: Option<String>,
    pub agent: String,
    pub harness: String,
    pub outcome: Option<String>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status {
    pub agents: Vec<AgentRow>,
    pub runs: Vec<RunRow>,
}

/// Open agents and unfinished runs; `all` adds ended agents and finished runs.
pub fn read(conn: &Connection, all: bool) -> rusqlite::Result<Status> {
    read_with(conn, all, if all { RunWindow::All } else { RunWindow::Open })
}

/// Which finished runs `status` lists besides the unfinished ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunWindow {
    Open,
    /// Runs that ended at or after this time, ms.
    Since(u64),
    All,
}

/// `status --metrics` keeps a cleaned-up worker's spend for this long.
const METRICS_WINDOW_MS: u64 = 24 * 3600 * 1000;

/// `all_agents` adds ended agents, `runs` picks the finished runs. A
/// cleaned-up worker's run row stays in the database, so its spend stays readable.
pub fn read_with(
    conn: &Connection,
    all_agents: bool,
    runs: RunWindow,
) -> rusqlite::Result<Status> {
    let mut st = conn.prepare(
        "SELECT a.name, a.role, a.kind, p.slug, a.ended_ms IS NULL FROM agent a \
         LEFT JOIN project p ON p.id = a.project_id \
         WHERE (?1 OR a.ended_ms IS NULL) ORDER BY a.id",
    )?;
    let agents = st
        .query_map([all_agents], |r| {
            Ok(AgentRow {
                name: r.get(0)?,
                role: r.get(1)?,
                kind: r.get(2)?,
                project: r.get(3)?,
                open: r.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let mut st = conn.prepare(
        "SELECT r.workflow_id, a.name, r.harness, r.outcome, r.tokens_in, r.tokens_out, r.cost_usd \
         FROM run r JOIN agent a ON a.id = r.agent_id \
         WHERE (?1 OR r.ended_ms IS NULL OR r.ended_ms >= ?2) ORDER BY r.id",
    )?;
    let (all_runs, since) = match runs {
        RunWindow::Open => (false, i64::MAX),
        RunWindow::Since(ms) => (false, ms as i64),
        RunWindow::All => (true, 0),
    };
    let runs = st
        .query_map(params![all_runs, since], |r| {
            Ok(RunRow {
                workflow: r.get(0)?,
                agent: r.get(1)?,
                harness: r.get(2)?,
                outcome: r.get(3)?,
                tokens_in: r.get(4)?,
                tokens_out: r.get(5)?,
                cost_usd: r.get(6)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(Status { agents, runs })
}

/// Text for the terminal. `metrics` adds tokens and cost per run; a run whose
/// cost was never reported prints `unknown`, never `0`. `status --metrics`
/// lists the runs that ended in the last 24 h too, so a cleaned-up worker keeps
/// its spend for a day; `--all` lists every run.
pub fn render(s: &Status, metrics: bool) -> String {
    let mut out = String::new();
    for a in &s.agents {
        out.push_str(&format!(
            "{} {} {} {}{}\n",
            a.name,
            a.role,
            a.kind.as_deref().unwrap_or("-"),
            a.project.as_deref().unwrap_or("-"),
            if a.open { "" } else { " (ended)" }
        ));
    }
    if metrics {
        for r in &s.runs {
            let cost = r
                .cost_usd
                .map(|c| format!("${c:.2}"))
                .unwrap_or_else(|| "unknown".into());
            out.push_str(&format!(
                "run {} {} {} in={} out={} cost={}\n",
                r.workflow.as_deref().unwrap_or("-"),
                r.agent,
                r.outcome.as_deref().unwrap_or("running"),
                r.tokens_in.unwrap_or(0),
                r.tokens_out.unwrap_or(0),
                cost
            ));
        }
    }
    out
}

pub fn status_command(env: &crate::cli::Env, args: &[String]) -> Result<String, crate::cli::CliError> {
    let err = |e: String| crate::cli::CliError::new(1, e);
    let mut all = false;
    let mut metrics = false;
    for a in args {
        match a.as_str() {
            "--all" => all = true,
            "--metrics" => metrics = true,
            other => {
                return Err(err(format!(
                    "usage: status [--all] [--metrics] (got '{other}')"
                )))
            }
        }
    }
    let conn = crate::db::open(&env.runs_dir.join(crate::db::path::DB_FILE))
        .map_err(|e| err(e.to_string()))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let runs = match (all, metrics) {
        (true, _) => RunWindow::All,
        (false, true) => RunWindow::Since(now.saturating_sub(METRICS_WINDOW_MS)),
        (false, false) => RunWindow::Open,
    };
    let s = read_with(&conn, all, runs).map_err(|e| err(e.to_string()))?;
    Ok(render(&s, metrics).trim_end().to_string())
}

/// The module's seam in `cli::MODULES`: `status` reads the database directly.
pub const INSTALL: crate::cli::Install = install;

fn install(cli: &mut crate::cli::Registry, _: &mut crate::daemon::Daemon) -> Result<(), String> {
    cli.register(crate::cli::Command {
        name: "status",
        help: "agents and runs [--all] [--metrics]",
        run: status_command,
    })
}
