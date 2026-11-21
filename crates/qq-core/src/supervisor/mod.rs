//! Supervisor (R13): scheduled jobs plus a short-lived read-only agent.
//!
//! Jobs: the computational sensors first (the ported auditor, static analysis,
//! architecture rules, the repeated-rule counter), then the inferential ones
//! (conversation scoring against the rubrics, the agent's question to the
//! Lead). A score of 0 files one proposal through the backlog. The agent
//! writes no code, gives no order and accepts nothing ([`agent`]). The scorer
//! and the agent are seams, so the checks run with fakes and no model call.

use std::path::{Path, PathBuf};

use crate::cli::{CliError, Command, Env, Install, Registry};
use crate::daemon::Daemon;

pub mod agent;
pub mod job;
pub mod rubric;
pub mod sensors;

pub const INSTALL: Install = install;

pub const USAGE: &str = "usage: quaqua supervisor score [--scorer <program>] [--rubrics <file>]";

/// Program that scores one rubric: prompt on stdin, JSON `{"score":0|1|2,"quote":"..."}` on stdout.
pub const SCORER_ENV: &str = "QUAQUA_SUPERVISOR_SCORER";

fn install(registry: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "supervisor",
        help: "supervisor score: run the conversation scoring job once",
        run: supervisor_cmd,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn usage_err(msg: &str) -> CliError {
    CliError::new(2, format!("{msg}\n{USAGE}"))
}

fn supervisor_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    match args.first().map(String::as_str) {
        Some("score") => {}
        Some("--help" | "-h") => return Ok(USAGE.to_string()),
        Some(o) => return Err(usage_err(&format!("unknown supervisor command '{o}'"))),
        None => return Err(usage_err("missing command")),
    }
    let mut scorer: Option<PathBuf> = std::env::var_os(SCORER_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let mut rubrics: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        let slot = match args[i].as_str() {
            "--scorer" => &mut scorer,
            "--rubrics" => &mut rubrics,
            o => return Err(usage_err(&format!("unknown supervisor option '{o}'"))),
        };
        let v = args
            .get(i + 1)
            .ok_or_else(|| usage_err(&format!("{} requires a value", args[i])))?;
        *slot = Some(PathBuf::from(v));
        i += 2;
    }
    let scorer = scorer.ok_or_else(|| {
        usage_err(&format!("no scorer: pass --scorer <program> or set {SCORER_ENV}"))
    })?;
    let rubric_path = rubrics.unwrap_or_else(|| rubric::rubrics_path(&env.runs_dir));
    let set = rubric::load_or_default(&rubric_path).map_err(|e| CliError::new(1, e))?;
    let mut conn = crate::db::open(&env.runs_dir.join(crate::db::path::DB_FILE))
        .map_err(|e| CliError::new(1, format!("cannot open the database: {e}")))?;
    let r = job::score_runs(
        &mut conn,
        &set,
        &job::ExecScorer { program: scorer },
        &agent::TemplateAgent,
        now_ms(),
    )
    .map_err(|e| CliError::new(1, e))?;
    let mut out = format!(
        "scored {} low {} proposals {:?}",
        r.scored, r.low, r.proposals
    );
    for e in &r.errors {
        out.push_str(&format!("\nerror: {e}"));
    }
    Ok(out)
}

/// Where the owner's rubric set lives, next to the runs directory.
pub fn default_rubrics_path(runs_dir: &Path) -> PathBuf {
    rubric::rubrics_path(runs_dir)
}
