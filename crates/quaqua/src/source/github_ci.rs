//! GitHub CI source: `quaqua source github-ci poll --repo <owner/name>` (spec §9, R7).
//!
//! One poll reads the named repo's most recent runs with
//! `gh run list --repo <owner/name> --json databaseId,headBranch,conclusion,url,workflowName --limit 50`
//! and records one `ci_failed` trigger for each run that failed on a `quaqua/*`
//! branch whose thread already exists. Everything else is dropped here, at the
//! fast path, before any write.
//!
//! The repo is an explicit argument, never the caller's cwd: a timer started from
//! `$HOME` must poll the repo it was configured for, and a poll started inside
//! another checkout must not silently read that checkout's remote.
//!
//! Idempotence: every trigger carries `dedupe_key = "gh-run:<databaseId>"`, so a
//! repeated poll, a second poll racing the first, or a poll after a crash
//! records each run once. The read position (`source_state.source = 'github-ci'`,
//! the largest run id ever seen) is a watermark, never a filter: a run whose
//! thread appears later is still emitted by a later poll, and a crash between
//! the trigger write and the watermark write loses nothing.
//!
//! Wake: a newly recorded trigger nudges every live subscriber of its thread
//! through the same path `send_mail` uses (`wake_idle_consumer`). A duplicate
//! run records nothing and wakes nobody.
//!
//! A transient failed read — non-zero exit, unparsable output — records nothing,
//! leaves the watermark untouched and exits 0: the next tick retries. A `gh`
//! that is not on PATH is a permanent misconfiguration, not a network blip, so
//! it exits 1 with an actionable error instead of polling nothing forever.

use crate::bus::{FarmBus, NewTrigger};
use crate::herdr::HerdrOps;
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
/// a transient read failure), 1 database/write error or `gh` not on PATH, 2
/// legacy database.
pub fn run_poll<H: HerdrOps>(runs_dir: &Path, repo: &str, herdr: &H) -> Result<i32, String> {
    let db_path = runs_dir.join("quaqua.db");

    let runs = match fetch_runs(repo) {
        Ok(runs) => runs,
        Err(FetchError::GhMissing) => {
            eprintln!(
                "error: cannot run gh: `gh` was not found on PATH; install the GitHub CLI or fix PATH"
            );
            return Ok(1);
        }
        Err(FetchError::Other(e)) => {
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
    let mut wake_threads: Vec<String> = Vec::new();
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
            if !wake_threads.iter().any(|t| t == branch) {
                wake_threads.push(branch.to_string());
            }
        }
    }

    // R7: the trigger reaches the owner through the subscription, then a wake.
    // A duplicate run recorded nothing, so it wakes nobody.
    for thread_id in &wake_threads {
        wake_live_subscribers(&bus, thread_id, herdr);
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

/// Why the run list could not be read.
enum FetchError {
    /// `gh` is not on PATH: a permanent misconfiguration, not a network blip.
    GhMissing,
    /// Any other read failure: transient, retried by the next tick.
    Other(String),
}

/// Reads the recent runs of `repo`. Any failure is a failed read.
fn fetch_runs(repo: &str) -> Result<Vec<GhRun>, FetchError> {
    let output = Command::new("gh")
        .args([
            "run",
            "list",
            "--repo",
            repo,
            "--json",
            RUN_FIELDS,
            "--limit",
            &RUN_LIMIT.to_string(),
        ])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FetchError::GhMissing
            } else {
                FetchError::Other(format!("cannot run gh: {e}"))
            }
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(FetchError::Other(format!(
            "gh run list exited {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| FetchError::Other(format!("cannot parse gh run list output: {e}")))
}

/// Nudges every live subscriber of `thread_id` through the P3 wake path.
///
/// Best-effort, like the wake after `send_mail`: a herdr failure must not turn a
/// recorded trigger into a failed poll. `wake_idle_consumer` re-checks the
/// consumer's unread set and status, so an already-announced batch sends nothing.
fn wake_live_subscribers<H: HerdrOps>(bus: &FarmBus, thread_id: &str, herdr: &H) {
    let now = crate::bus::now_ms();
    let Ok(subscriptions) = bus.get_subscriptions(thread_id) else {
        return;
    };
    for (consumer, until_ms) in subscriptions {
        if until_ms >= now {
            let _ = crate::bus::events::wake_idle_consumer(bus, &consumer, herdr);
        }
    }
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
