//! GitHub CI source: `quaqua source github-ci poll` (spec §9, R7).
//!
//! One poll reads the engine repo's most recent runs with
//! `gh run list --json databaseId,headBranch,conclusion,url,workflowName --limit 50`
//! and records one `ci_failed` trigger for each run that failed on a `quaqua/*`
//! branch whose thread already exists. Everything else is dropped here, at the
//! fast path, before any write.
//!
//! Idempotence: every trigger carries `dedupe_key = "gh-run:<databaseId>"`, so a
//! repeated poll, a second poll racing the first, or a poll after a crash
//! records each run once. The read position (`source_state.source = 'github-ci'`,
//! the largest run id ever seen) is a watermark, never a filter: a run whose
//! thread appears later is still emitted by a later poll, and a crash between
//! the trigger write and the watermark write loses nothing.
//!
//! A failed read — `gh` missing, non-zero exit, unparsable output — records
//! nothing, leaves the watermark untouched and exits 0: the next tick retries.

use crate::bus::{FarmBus, NewTrigger};
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use std::process::Command;

/// `triggers.source` value for this source.
pub const SOURCE: &str = "github-ci";
/// Thread key prefix CI failures are reported on.
const BRANCH_PREFIX: &str = "quaqua/";
/// Runs requested per poll.
const RUN_LIMIT: u32 = 50;
/// Fields requested from `gh run list`.
const RUN_FIELDS: &str = "databaseId,headBranch,conclusion,url,workflowName";

/// One entry of `gh run list --json databaseId,headBranch,conclusion,url,workflowName`.
#[derive(Debug, serde::Deserialize)]
struct GhRun {
    #[serde(rename = "databaseId")]
    database_id: Option<u64>,
    #[serde(rename = "headBranch")]
    head_branch: Option<String>,
    conclusion: Option<String>,
    url: Option<String>,
    #[serde(rename = "workflowName")]
    workflow_name: Option<String>,
}

/// Polls GitHub CI once. Returns the process exit code.
///
/// Exit codes follow the CLI contract: 0 polled (including a skipped poll after
/// a failed read), 1 database or write error, 2 legacy database.
pub fn run_poll(runs_dir: &Path) -> Result<i32, String> {
    let db_path = runs_dir.join("quaqua.db");

    let runs = match fetch_runs() {
        Ok(runs) => runs,
        Err(e) => {
            eprintln!("warning: github-ci poll skipped: {e}");
            return Ok(0);
        }
    };

    let bus = match FarmBus::open(&db_path) {
        Ok(bus) => bus,
        Err(e) => {
            eprintln!("error opening bus at {}: {e}", db_path.display());
            return Ok(if crate::bus::is_legacy_db_error(&e) {
                2
            } else {
                1
            });
        }
    };
    let conn = crate::engine::sqlite::open_conn(&db_path)
        .map_err(|e| format!("cannot open {}: {e}", db_path.display()))?;

    let previous = read_position(&conn)?;

    let mut runs: Vec<(u64, GhRun)> = runs
        .into_iter()
        .filter_map(|run| run.database_id.map(|id| (id, run)))
        .collect();
    runs.sort_by_key(|(id, _)| *id);
    let seen = runs.len();

    let mut recorded = 0usize;
    let mut max_seen = 0u64;
    for (id, run) in &runs {
        max_seen = max_seen.max(*id);
        if run.conclusion.as_deref() != Some("failure") {
            continue;
        }
        let Some(branch) = run.head_branch.as_deref() else {
            continue;
        };
        if !branch.starts_with(BRANCH_PREFIX) {
            continue;
        }
        if !thread_exists(&conn, branch)? {
            continue;
        }

        let workflow = run.workflow_name.as_deref().unwrap_or("workflow");
        let mut trigger = NewTrigger::new(
            SOURCE,
            "ci_failed",
            "github",
            branch,
            format!("{workflow} failed on {branch}"),
        )
        .with_dedupe_key(format!("gh-run:{id}"));
        if let Some(url) = run.url.as_deref() {
            trigger = trigger.with_ref(url);
        }
        if bus
            .record_trigger(&trigger)
            .map_err(|e| format!("cannot record trigger for run {id}: {e}"))?
        {
            recorded += 1;
        }
    }

    // The 50-run window is the only place a run can be missed: everything older
    // than the newest 50 is invisible to this poll. Say so instead of guessing.
    if let Some((oldest, _)) = runs.first() {
        if previous > 0 && previous < *oldest {
            eprintln!(
                "warning: github-ci window gap: runs after {previous} and before {oldest} were never seen"
            );
        }
    }

    let position = previous.max(max_seen);
    if position != previous {
        write_position(&conn, position)?;
    }

    println!("github-ci: {recorded} new trigger(s), {seen} run(s) seen, position {position}");
    Ok(0)
}

/// Reads the recent runs of the engine repo. Any failure is a failed read.
fn fetch_runs() -> Result<Vec<GhRun>, String> {
    let output = Command::new("gh")
        .args([
            "run",
            "list",
            "--json",
            RUN_FIELDS,
            "--limit",
            &RUN_LIMIT.to_string(),
        ])
        .output()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "gh run list exited {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cannot parse gh run list output: {e}"))
}

fn read_position(conn: &Connection) -> Result<u64, String> {
    let value: Option<String> = crate::engine::sqlite::retry_while_busy(|| {
        conn.query_row(
            "SELECT value FROM source_state WHERE source = ?1",
            [SOURCE],
            |row| row.get(0),
        )
        .optional()
    })
    .map_err(|e| format!("cannot read source_state: {e}"))?;
    Ok(value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0))
}

fn write_position(conn: &Connection, value: u64) -> Result<(), String> {
    crate::engine::sqlite::retry_while_busy(|| {
        conn.execute(
            "INSERT INTO source_state (source, value) VALUES (?1, ?2) \
             ON CONFLICT(source) DO UPDATE SET value = excluded.value",
            rusqlite::params![SOURCE, value.to_string()],
        )
        .map(|_| ())
    })
    .map_err(|e| format!("cannot write source_state: {e}"))
}

fn thread_exists(conn: &Connection, thread_id: &str) -> Result<bool, String> {
    let found: Option<i64> = crate::engine::sqlite::retry_while_busy(|| {
        conn.query_row("SELECT 1 FROM threads WHERE id = ?1", [thread_id], |row| {
            row.get(0)
        })
        .optional()
    })
    .map_err(|e| format!("cannot read thread {thread_id}: {e}"))?;
    Ok(found.is_some())
}
