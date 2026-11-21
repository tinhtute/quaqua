//! The task graph (R9, R22): parents a task waits for, the disposition it
//! carries, and the states a worker's report leaves it in.
//!
//! A task starts only when every parent is `verified`. The disposition is its
//! own column: nothing here reads the task's kind. A report that ends in
//! REOPEN_REQUEST, DEPENDENCY_REQUEST or BLOCKED leaves the task in a state of
//! that name; the Lead reads the state, so no topology edge is needed.

use rusqlite::{params, Connection, OptionalExtension};

pub const DISPOSITIONS: &[&str] = &["engineer", "architect", "reviewer", "scout"];
pub const DEFAULT_DISPOSITION: &str = "engineer";

pub const VERIFIED: &str = "verified";
pub const BLOCKED: &str = "blocked";
pub const REOPEN_REQUEST: &str = "reopen_request";
pub const DEPENDENCY_REQUEST: &str = "dependency_request";

/// The task state a run outcome leaves, or none when the task stays as it is.
/// A task that was already satisfied (`no_changes`) counts as verified.
pub fn end_state(outcome: &str) -> Option<&'static str> {
    match outcome {
        "verified" | "no_changes" => Some(VERIFIED),
        "reopen_request" => Some(REOPEN_REQUEST),
        "dependency_request" => Some(DEPENDENCY_REQUEST),
        "reported_blocked" | "blocked" => Some(BLOCKED),
        _ => None,
    }
}

/// Writes the state a run outcome leaves on its task. Idempotent.
pub fn settle(conn: &Connection, task_id: i64, outcome: &str) -> rusqlite::Result<()> {
    if let Some(state) = end_state(outcome) {
        conn.execute("UPDATE task SET state = ?2 WHERE id = ?1", params![task_id, state])?;
    }
    Ok(())
}

/// The task's disposition, whatever its kind.
pub fn disposition(conn: &Connection, task_id: i64) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT disposition FROM task WHERE id = ?1", [task_id], |r| r.get(0))
        .optional()
}

/// Declares `parent` as a parent of `task`. Refuses an unknown task, a task
/// in another project, and an edge that would close a cycle. A repeat is a no-op.
pub fn add_parent(conn: &Connection, task: i64, parent: i64) -> Result<(), String> {
    let project = |id: i64| -> Result<i64, String> {
        conn.query_row("SELECT project_id FROM task WHERE id = ?1", [id], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no task {id}"))
    };
    if task == parent {
        return Err(format!("task {task} cannot be its own parent"));
    }
    if project(task)? != project(parent)? {
        return Err(format!("task {parent} belongs to another project than task {task}"));
    }
    // A cycle closes when the new parent already descends from the task.
    let cycle: bool = conn
        .query_row(
            "WITH RECURSIVE below(id) AS (
                 SELECT task_id FROM task_dep WHERE parent_id = ?1
                 UNION
                 SELECT d.task_id FROM task_dep d JOIN below b ON d.parent_id = b.id
             ) SELECT EXISTS (SELECT 1 FROM below WHERE id = ?2)",
            params![task, parent],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if cycle {
        return Err(format!("task {parent} already depends on task {task}: a cycle"));
    }
    conn.execute(
        "INSERT OR IGNORE INTO task_dep (task_id, parent_id) VALUES (?1, ?2)",
        params![task, parent],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// The parents of `task` that are not `verified` yet; empty means it may start.
pub fn unverified_parents(conn: &Connection, task: i64) -> rusqlite::Result<Vec<i64>> {
    let mut st = conn.prepare(
        "SELECT p.id FROM task_dep d JOIN task p ON p.id = d.parent_id \
         WHERE d.task_id = ?1 AND p.state <> ?2 ORDER BY p.id",
    )?;
    let rows = st.query_map(params![task, VERIFIED], |r| r.get(0))?;
    rows.collect()
}
