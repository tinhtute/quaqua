//! The review pair rule: a reviewer that runs on the builder's model is
//! refused, whatever the harness. Each kind carries one model id in the config,
//! so the two ids are compared as written (trimmed, case-insensitive).

use rusqlite::Connection;

use crate::authority::{self, Decision};
use crate::config;

/// Caller recorded on the refusal row (the daemon, not an agent).
const CALLER: &str = "pipeline";

/// Ok when the reviewer kind runs on another model than the builder kind.
/// On a shared model the refusal is journaled (one `refusal` row whose reason
/// names the builder kind, the reviewer kind and the model) and returned as
/// the cause. An unreadable config or journal refuses too (fail closed).
pub fn check(conn: &Connection, builder_kind: &str, reviewer_kind: &str) -> Result<(), String> {
    let b = config::resolve_spawn(conn, builder_kind)
        .map_err(|e| format!("builder kind '{builder_kind}': {e:?}"))?;
    let r = config::resolve_spawn(conn, reviewer_kind)
        .map_err(|e| format!("reviewer kind '{reviewer_kind}': {e:?}"))?;
    if !b.model.trim().eq_ignore_ascii_case(r.model.trim()) {
        return Ok(());
    }
    let reason = format!(
        "review refused: builder kind '{builder_kind}' and reviewer kind '{reviewer_kind}' share model '{}'",
        b.model.trim()
    );
    match authority::refuse(conn, CALLER, "review", reviewer_kind, &reason) {
        Ok(Decision::Refused(_)) | Ok(Decision::Allowed) => Err(reason),
        Err(e) => Err(format!("{reason} (not journaled: {e})")),
    }
}
