//! Daemon schedule (R24, T62c): the daemon's own loop runs what a schedule row
//! says is due, and nothing listens for an outside push. A row names a source
//! or a Supervisor job and its interval.
//!
//! Sources are pulled by [`pull_due`] on their rows, and on demand by the
//! `source.pull` verb ([`crate::source::pull_now`]); both file each item
//! once. The Supervisor jobs are [`SCORE_JOB`] (scoring, only runs that ended
//! since the last pass, and only once a scorer program is set as the
//! `limits.supervisor_scorer` setting; unset is the shipped default) and
//! [`SENSORS_JOB`] (the computational sensors of loop 2 in the order of the
//! sensor table). [`run_due`] runs the due pulls, then scoring, then the
//! sensors; [`spawn_runner`] calls it from a daemon thread. The migration
//! seeds the job rows; [`ensure_sources`] adds the source rows at daemon
//! start. No socket, webhook or external-event endpoint lives here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{params, Connection};

use super::Ctx;
use crate::auditor::AuditorRunOptions;
use crate::source::{file_once, FailureSource, IdeaInbox, Source, FAILURE_SOURCE, IDEA_SOURCE};
use crate::supervisor::agent::{AgentRunner, TemplateAgent};
use crate::supervisor::job::{self, ExecScorer, Scorer, Sensors, SCORE_JOB};
use crate::supervisor::rubric::{self, Rubric};

/// The sensors job: loop 2's computational sensors in the order of the sensor table.
pub const SENSORS_JOB: &str = "supervisor.sensors";

/// The setting that names the scorer program. Unset: scoring does nothing. It lives
/// under `limits` because `config set` and team.toml accept only the known sections.
pub const SCORER_SETTING: &str = "limits.supervisor_scorer";

/// How often a source is pulled when the daemon adds its row.
const SOURCE_INTERVAL_MS: i64 = 300_000;

/// The longest the runner sleeps before it looks at the rows again.
const POLL: Duration = Duration::from_secs(1);

fn is_job(name: &str) -> bool {
    name == SCORE_JOB || name == SENSORS_JOB
}

/// Adds a schedule row, due at `now_ms`; an existing row is left as it is.
pub fn add(conn: &Connection, name: &str, interval_ms: i64, now_ms: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO schedule (name, interval_ms, next_due_ms) VALUES (?1, ?2, ?3)",
        params![name, interval_ms, now_ms],
    )?;
    Ok(())
}

/// Names of the schedules due at `now_ms`, oldest due first.
pub fn due(conn: &Connection, now_ms: i64) -> rusqlite::Result<Vec<String>> {
    let mut st = conn
        .prepare("SELECT name FROM schedule WHERE next_due_ms <= ?1 ORDER BY next_due_ms, name")?;
    let rows = st.query_map([now_ms], |r| r.get(0))?;
    rows.collect()
}

/// The earliest next-due time, for the daemon's wait; none with no schedule.
pub fn next_deadline(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT MIN(next_due_ms) FROM schedule", [], |r| r.get(0))
}

/// Pulls every source whose schedule is due, files its new items once and moves
/// the schedule by one interval from `now_ms`. Returns the ids of the proposals
/// filed. The schedule moves before the pull, so one broken source cannot
/// spin; its error comes back after the other sources ran.
pub fn pull_due(
    conn: &mut Connection,
    sources: &[&dyn Source],
    now_ms: i64,
) -> Result<Vec<i64>, String> {
    let mut filed = Vec::new();
    let mut first_err = None;
    for name in due(conn, now_ms).map_err(|e| e.to_string())? {
        if is_job(&name) {
            continue;
        }
        conn.execute(
            "UPDATE schedule SET next_due_ms = ?2 + interval_ms, last_run_ms = ?2 WHERE name = ?1",
            params![name, now_ms],
        )
        .map_err(|e| e.to_string())?;
        let Some(src) = sources.iter().find(|s| s.name() == name) else {
            continue;
        };
        match src.pull(conn) {
            Ok(items) => {
                for item in &items {
                    match file_once(conn, &name, item, now_ms) {
                        Ok(Some(id)) => filed.push(id),
                        Ok(None) => {}
                        Err(e) => {
                            first_err.get_or_insert(format!("source {name}: {e}"));
                        }
                    }
                }
            }
            Err(e) => {
                first_err.get_or_insert(format!("source {name}: {e}"));
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(filed),
    }
}

/// Adds the rows of the two shipped sources, first pull one interval from `now_ms`.
/// An existing row is left as it is.
pub fn ensure_sources(conn: &Connection, now_ms: i64) -> rusqlite::Result<()> {
    for name in [FAILURE_SOURCE, IDEA_SOURCE] {
        add(conn, name, SOURCE_INTERVAL_MS, now_ms + SOURCE_INTERVAL_MS)?;
    }
    Ok(())
}

/// What the due jobs need. `scorer` is none while no scorer is configured.
pub struct Jobs<'a> {
    pub sources: &'a [&'a dyn Source],
    pub sensors: &'a Sensors,
    pub rubrics: &'a [Rubric],
    pub scorer: Option<&'a dyn Scorer>,
    pub agent: &'a dyn AgentRunner,
}

/// What one pass did.
#[derive(Debug, Default)]
pub struct Ran {
    /// Proposals filed by the pulls, scoring and the sensors.
    pub proposals: Vec<i64>,
    /// Job rows that ran, in order.
    pub jobs: Vec<&'static str>,
    pub errors: Vec<String>,
}

/// The scorer program from the `limits.supervisor_scorer` setting; none when unset or empty.
pub fn configured_scorer(conn: &Connection) -> Option<ExecScorer> {
    let v = crate::config::setting(conn, SCORER_SETTING).ok().flatten()?;
    let program = v.as_str().map(str::trim).filter(|s| !s.is_empty())?;
    Some(ExecScorer {
        program: PathBuf::from(program),
    })
}

/// Moves a job row one interval from `now_ms` when it is due; true when it was.
fn claim(conn: &Connection, name: &str, now_ms: i64) -> Result<bool, String> {
    let n = conn
        .execute(
            "UPDATE schedule SET next_due_ms = ?2 + interval_ms, last_run_ms = ?2 \
             WHERE name = ?1 AND next_due_ms <= ?2",
            params![name, now_ms],
        )
        .map_err(|e| e.to_string())?;
    Ok(n == 1)
}

/// Runs what is due at `now_ms`: the source pulls, then scoring, then the
/// sensors. Each job's row moves before it runs, so a failing job cannot spin.
/// Scoring with no scorer configured does nothing and leaves its cursor alone.
/// Every job runs; errors are collected in the result.
pub fn run_due(conn: &mut Connection, jobs: &Jobs<'_>, now_ms: i64) -> Result<Ran, String> {
    let mut ran = Ran::default();
    match pull_due(conn, jobs.sources, now_ms) {
        Ok(ids) => ran.proposals.extend(ids),
        Err(e) => ran.errors.push(e),
    }
    if claim(conn, SCORE_JOB, now_ms)? {
        if let Some(scorer) = jobs.scorer {
            ran.jobs.push(SCORE_JOB);
            match job::score_new_runs(conn, jobs.rubrics, scorer, jobs.agent, now_ms) {
                Ok(r) => {
                    ran.proposals.extend(r.proposals);
                    ran.errors.extend(r.errors);
                }
                Err(e) => ran.errors.push(format!("scoring: {e}")),
            }
        }
    }
    if claim(conn, SENSORS_JOB, now_ms)? {
        ran.jobs.push(SENSORS_JOB);
        match job::run_sensors(conn, jobs.sensors, now_ms) {
            Ok(r) => ran.proposals.extend(r.sensor_proposals),
            Err(e) => ran.errors.push(format!("sensors: {e}")),
        }
    }
    Ok(ran)
}

fn clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// One pass of the daemon loop against the real workspace: sources, rubrics
/// and sensors of `runs_dir`'s workspace, the scorer from the setting.
/// Every error of the pass comes back, none is dropped.
fn tick(runs_dir: &std::path::Path) -> Vec<String> {
    tick_at(runs_dir, clock_ms())
}

/// One pass of the daemon loop at `now`; every error of the pass comes back.
pub fn tick_at(runs_dir: &std::path::Path, now: i64) -> Vec<String> {
    tick_inner(runs_dir, now).unwrap_or_else(|e| vec![e])
}

fn tick_inner(runs_dir: &std::path::Path, now: i64) -> Result<Vec<String>, String> {
    let workspace = runs_dir.parent().ok_or("runs dir has no workspace")?;
    let mut conn = crate::db::open(&runs_dir.join(crate::db::path::DB_FILE))
        .map_err(|e| e.to_string())?;
    if !due(&conn, now).map_err(|e| e.to_string())?.is_empty() {
        let inbox = IdeaInbox::new(workspace);
        let sources: [&dyn Source; 2] = [&FailureSource, &inbox];
        let sensors = Sensors {
            root: workspace.to_path_buf(),
            // Deterministic prechecks only: a model-run audit spends tokens and needs the owner.
            auditor: AuditorRunOptions {
                dry_run: true,
                ..Default::default()
            },
            static_checks: job::default_static_checks(workspace),
            arch_rules: job::default_arch_rules(),
        };
        // A bad rubrics file stops scoring only: pulls and sensors still run, and the
        // error is reported once per scoring interval, not on every tick.
        let mut errors = Vec::new();
        let (rubrics, rubrics_ok) = match rubric::load_or_default(&rubric::rubrics_path(runs_dir)) {
            Ok(r) => (r, true),
            Err(e) => {
                errors.push(format!("scoring: rubrics: {e}"));
                (Vec::new(), false)
            }
        };
        let scorer = configured_scorer(&conn).filter(|_| rubrics_ok);
        if !rubrics_ok
            && (configured_scorer(&conn).is_none()
                || !due(&conn, now).map_err(|e| e.to_string())?.iter().any(|n| n == SCORE_JOB))
        {
            errors.clear();
        }
        let jobs = Jobs {
            sources: &sources,
            sensors: &sensors,
            rubrics: &rubrics,
            scorer: scorer.as_ref().map(|s| s as &dyn Scorer),
            agent: &TemplateAgent,
        };
        errors.extend(run_due(&mut conn, &jobs, now)?.errors);
        return Ok(errors);
    }
    Ok(Vec::new())
}

/// The daemon's schedule thread: until `stop`, it runs the due pulls and
/// Supervisor jobs, then waits for the earliest next due time (at most a second).
pub fn spawn_runner(ctx: Weak<Ctx>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    // The source rows are added before the thread starts, so this write cannot
    // race with a verb that writes right after the daemon is up.
    if let Some(c) = ctx.upgrade() {
        let db = c.runs_dir.join(crate::db::path::DB_FILE);
        if let Ok(conn) = crate::db::open(&db) {
            let _ = ensure_sources(&conn, clock_ms());
        }
    }
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let Some(c) = ctx.upgrade() else { return };
            let runs_dir = c.runs_dir.clone();
            drop(c);
            for e in tick(&runs_dir) {
                eprintln!("quaqua daemon: schedule: {e}");
            }
            let wait = crate::db::open(&runs_dir.join(crate::db::path::DB_FILE))
                .ok()
                .and_then(|c| next_deadline(&c).ok().flatten())
                .map_or(POLL, |d| {
                    Duration::from_millis((d - clock_ms()).clamp(0, POLL.as_millis() as i64) as u64)
                });
            let mut left = wait.max(Duration::from_millis(20));
            while !stop.load(Ordering::SeqCst) && !left.is_zero() {
                let d = left.min(Duration::from_millis(20));
                std::thread::sleep(d);
                left -= d;
            }
        }
    })
}
