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
    /// Reasons the owner should look: nothing is running and nothing has happened.
    pub needs_you: Vec<String>,
}

/// Default minutes of silence, with no live worker, before status flags it.
pub const IDLE_MINUTES: u64 = 10;
/// Setting key that overrides [`IDLE_MINUTES`]. It lives in the existing
/// `timers` section, so `config set` and a `[timers]` table accept it. A value
/// that is not a whole number falls back to the default; `0` is raised to 1.
pub const IDLE_SETTING: &str = "timers.status_idle_minutes";

/// Open agents and unfinished runs; `all` adds ended agents and finished runs.
pub fn read(conn: &Connection, all: bool) -> rusqlite::Result<Status> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    read_at(conn, all, now_ms)
}

/// Flag when no worker (lead or peer) is live and the last event (mail, run or
/// agent start/end) is older than the idle threshold. Whether a CoS turn is
/// open is not checked: there is no per-turn signal in the database. A database with no events
/// at all is quiet: nothing has happened yet.
fn idle_reason(conn: &Connection, now_ms: i64) -> rusqlite::Result<Option<String>> {
    let live: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent WHERE ended_ms IS NULL AND role IN ('lead', 'peer')",
        [],
        |r| r.get(0),
    )?;
    if live > 0 {
        return Ok(None);
    }
    let last: Option<i64> = conn.query_row(
        "SELECT MAX(t) FROM (SELECT MAX(created_ms) t FROM mail \
         UNION ALL SELECT MAX(started_ms) FROM run UNION ALL SELECT MAX(ended_ms) FROM run \
         UNION ALL SELECT MAX(started_ms) FROM agent UNION ALL SELECT MAX(ended_ms) FROM agent)",
        [],
        |r| r.get(0),
    )?;
    let Some(last) = last else { return Ok(None) };
    let minutes: u64 = conn
        .query_row(
            "SELECT value_json FROM setting WHERE key = ?1",
            [IDLE_SETTING],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(IDLE_MINUTES)
        .max(1);
    let idle_s = (now_ms - last).max(0) / 1000;
    Ok((idle_s >= minutes as i64 * 60)
        .then(|| format!("no worker live and no event for {} min", idle_s / 60)))
}

/// [`read`] with an explicit clock, for tests.
pub fn read_at(conn: &Connection, all: bool, now_ms: i64) -> rusqlite::Result<Status> {
    let mut st = conn.prepare(
        "SELECT a.name, a.role, a.kind, p.slug, a.ended_ms IS NULL FROM agent a \
         LEFT JOIN project p ON p.id = a.project_id \
         WHERE (?1 OR a.ended_ms IS NULL) ORDER BY a.id",
    )?;
    let agents = st
        .query_map([all], |r| {
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
        .query_map([all], |r| {
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
    let needs_you = idle_reason(conn, now_ms)?.into_iter().collect();
    Ok(Status {
        agents,
        runs,
        needs_you,
    })
}

/// Text for the terminal. `metrics` adds tokens and cost per run; a run whose
/// cost was never reported prints `unknown`, never `0`.
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
    for n in &s.needs_you {
        out.push_str(&format!("needs you: {n}\n"));
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

fn status_command(env: &crate::cli::Env, args: &[String]) -> Result<String, crate::cli::CliError> {
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
    let s = read(&conn, all).map_err(|e| err(e.to_string()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrate::migrate(&mut c).unwrap();
        c
    }

    const MIN: i64 = 60_000;

    #[test]
    fn status_flags_idle_with_no_worker_and_no_event() {
        let c = db();
        c.execute(
            "INSERT INTO agent (name, role, started_ms, ended_ms) VALUES ('w', 'lead', 0, 1000)",
            [],
        )
        .unwrap();
        let s = read_at(&c, false, 1000 + 11 * MIN).unwrap();
        assert_eq!(
            s.needs_you,
            vec!["no worker live and no event for 11 min".to_string()]
        );
        assert!(render(&s, false).contains("needs you: no worker live and no event for 11 min"));
        c.execute(
            "INSERT INTO setting (key, value_json) VALUES ('timers.status_idle_minutes', '30')",
            [],
        )
        .unwrap();
        assert!(read_at(&c, false, 1000 + 11 * MIN)
            .unwrap()
            .needs_you
            .is_empty());
    }

    #[test]
    fn status_quiet_when_worker_live_or_event_recent_or_empty() {
        let c = db();
        assert!(
            read_at(&c, false, 99 * MIN).unwrap().needs_you.is_empty(),
            "empty db"
        );
        c.execute(
            "INSERT INTO agent (name, role, started_ms) VALUES ('w', 'lead', 0)",
            [],
        )
        .unwrap();
        assert!(
            read_at(&c, false, 99 * MIN).unwrap().needs_you.is_empty(),
            "live worker"
        );
        c.execute("UPDATE agent SET ended_ms = 1000", []).unwrap();
        c.execute(
            "INSERT INTO mail (sender, recipient, body, created_ms) VALUES ('w', 'cos', 'hi', ?1)",
            [1000 + 5 * MIN],
        )
        .unwrap();
        assert!(
            read_at(&c, false, 1000 + 12 * MIN)
                .unwrap()
                .needs_you
                .is_empty(),
            "recent mail"
        );
        assert!(render(&read_at(&c, false, 1000 + 12 * MIN).unwrap(), false)
            .find("needs you")
            .is_none());
    }

    #[test]
    fn idle_setting_is_a_valid_config_key_and_honoured() {
        let c = db();
        c.execute(
            "INSERT INTO agent (name, role, started_ms, ended_ms) VALUES ('w', 'lead', 0, 1000)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO setting (key, value_json) VALUES (?1, '30')",
            [IDLE_SETTING],
        )
        .unwrap();
        // config's export refuses a setting whose section is not in SETTING_SECTIONS.
        let toml = crate::config::export_toml(&c)
            .unwrap_or_else(|e| panic!("{IDLE_SETTING} is not a config key: {e}"));
        assert!(toml.contains("status_idle_minutes = 30"), "{toml}");
        assert!(IDLE_SETTING.starts_with("timers."));
        assert!(read_at(&c, false, 1000 + 11 * MIN)
            .unwrap()
            .needs_you
            .is_empty());
        assert_eq!(
            read_at(&c, false, 1000 + 31 * MIN).unwrap().needs_you.len(),
            1
        );
    }

    #[test]
    fn idle_setting_zero_is_clamped_and_junk_falls_back() {
        let c = db();
        c.execute(
            "INSERT INTO agent (name, role, started_ms, ended_ms) VALUES ('w', 'lead', 0, 1000)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO setting (key, value_json) VALUES (?1, '0')",
            [IDLE_SETTING],
        )
        .unwrap();
        assert!(
            read_at(&c, false, 1000 + 30_000)
                .unwrap()
                .needs_you
                .is_empty(),
            "0 clamps to 1 min"
        );
        assert_eq!(read_at(&c, false, 1000 + MIN).unwrap().needs_you.len(), 1);
        c.execute("UPDATE setting SET value_json = '\"soon\"'", [])
            .unwrap();
        assert!(
            read_at(&c, false, 1000 + 9 * MIN)
                .unwrap()
                .needs_you
                .is_empty(),
            "junk -> 10"
        );
        assert_eq!(
            read_at(&c, false, 1000 + 10 * MIN).unwrap().needs_you.len(),
            1
        );
    }
}
