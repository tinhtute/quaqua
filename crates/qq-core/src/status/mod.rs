//! `status`: the farm view, read from the database only.

use rusqlite::Connection;

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
    read_with(conn, all, all)
}

/// `all_agents` adds ended agents, `all_runs` adds finished runs. A cleaned-up
/// worker's run row stays in the database, so its spend stays readable.
pub fn read_with(
    conn: &Connection,
    all_agents: bool,
    all_runs: bool,
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
         WHERE (?1 OR r.ended_ms IS NULL) ORDER BY r.id",
    )?;
    let runs = st
        .query_map([all_runs], |r| {
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
/// lists finished runs too, so a cleaned-up worker keeps its spend.
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
    let s = read_with(&conn, all, all || metrics).map_err(|e| err(e.to_string()))?;
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
