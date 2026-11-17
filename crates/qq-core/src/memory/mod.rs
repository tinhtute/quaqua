//! The hook verbs that run on the new binary: `memory hook pre|batch|session-start`
//! (the memory caps and the SessionStart behavior) and `inbox` (the CoS mail
//! digest hook). Hooks fail open: input they cannot read is "no decision".

mod caps;
mod drift;
mod inbox;

pub use caps::*;
pub use drift::{
    drift, drift_message, is_dispatch, session_start as record_session, snapshot, tracked,
};
pub use inbox::{inbox_as, inbox_cmd};

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::cli::{CliError, Command, Env, Install, Registry};
use crate::daemon::Daemon;
use crate::identity::{self, PaneOps};

/// The entry `cli::MODULES` lists.
pub const INSTALL: Install = install;

fn install(registry: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "memory",
        help: "memory hook pre|batch|session-start (Claude Code hooks, JSON on stdin)",
        run: memory_cmd,
    })?;
    registry.register(Command {
        name: "inbox",
        help: "inbox [--for NAME] [--peek] [--all] [--format plain|hook-ups|hook-stop]",
        run: inbox::inbox_cmd,
    })
}

/// The hook's JSON. Claude Code hands it over on a pipe it closes; a terminal
/// has no hook behind it, so nothing is read (a read would wait for typing).
fn stdin_text() -> String {
    let mut s = String::new();
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let _ = stdin.lock().read_to_string(&mut s);
    }
    s
}

pub fn memory_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    memory_cmd_with(env, args, stdin_text)
}

/// [`memory_cmd`] with the hook input supplied by `input`, read only for a
/// valid mode, so a caller that has no hook behind it never touches stdin.
pub fn memory_cmd_with(
    env: &Env,
    args: &[String],
    input: impl FnOnce() -> String,
) -> Result<String, CliError> {
    let mode = match args {
        [h, m] if h == "hook" => m.as_str(),
        _ => {
            return Err(CliError::new(
                2,
                "usage: quaqua memory hook pre|batch|session-start",
            ))
        }
    };
    let input = input();
    let out = match mode {
        "pre" => pre_decision(&input).map(|v| v.to_string()),
        "batch" => batch_decision(&input).map(|v| v.to_string()),
        "session-start" => {
            let ws = env
                .runs_dir
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let pane = identity::pane_from_env();
            session_start(&ws, &input, identity::cli_panes().as_ref(), pane.as_deref())
        }
        _ => {
            return Err(CliError::new(
                2,
                "usage: quaqua memory hook pre|batch|session-start",
            ))
        }
    };
    // Hooks never fail the turn: no decision is an empty answer.
    Ok(out.unwrap_or_default())
}

/// The SessionStart hook. Records the session's rule baseline (a `resume`
/// keeps the old one), prints the re-read list after a compaction, and re-tags
/// the CoS pane when the session runs in the workspace itself. Idempotent: a
/// repeat rewrites the same baseline and finds the pane already tagged.
pub fn session_start(
    ws_root: &Path,
    input: &str,
    panes: &dyn PaneOps,
    pane: Option<&str>,
) -> Option<String> {
    let v: Value = serde_json::from_str(input).unwrap_or_default();
    let sid = v.get("session_id").and_then(Value::as_str).unwrap_or("");
    let source = v.get("source").and_then(Value::as_str).unwrap_or("");
    let text = record_session(ws_root, sid, source);
    let cwd = v
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    if let Some(Err(e)) = identity::session_retag(panes, pane, &cwd, ws_root) {
        eprintln!("quaqua: CoS pane re-tag failed: {e}");
    }
    text
}
