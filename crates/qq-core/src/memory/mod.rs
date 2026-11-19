//! The hook verbs that run on the new binary: `memory hook pre|batch|session-start`
//! (the memory caps and the SessionStart behavior) and `inbox` (the CoS mail
//! digest hook). Hooks fail open: input they cannot read is "no decision".

mod caps;
mod drift;
mod inbox;
mod situation;
mod standing;

pub use caps::*;
pub use drift::{
    compact_hint, drift, drift_message, is_dispatch, session_start as record_session, snapshot, tracked,
};
pub use inbox::{inbox_as, inbox_cmd};
pub use standing::{render_standing, BUDGET as STANDING_BUDGET, HEADER as STANDING_HEADER};

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::{CliError, Command, Env, Install, Registry};
use crate::daemon::Daemon;
use crate::identity::{self, PaneOps};

/// The entry `cli::MODULES` lists.
pub const INSTALL: Install = install;

fn install(registry: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "memory",
        help: "memory hook pre|batch|session-start (Claude Code hooks, JSON on stdin) | situation",
        run: memory_cmd,
    })?;
    registry.register(Command {
        name: "inbox",
        help: "inbox [--for NAME] [--peek] [--all] [--format plain|hook-ups|hook-stop]",
        run: inbox::inbox_cmd,
    })
}

/// `quaqua memory situation`: the computed situation of this workspace, with
/// herdr statuses when herdr answers (a missing herdr only drops the status).
fn situation_cli(env: &Env) -> String {
    let ws = env.runs_dir.parent().unwrap_or_else(|| Path::new("."));
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mut s = situation::compute(ws, now_ms);
    let statuses = crate::herdr::HerdrClient::agent_list(&crate::herdr::CliHerdr::new())
        .ok()
        .filter(|r| r.ok)
        .and_then(|r| serde_json::from_str::<Value>(&r.body).ok())
        .and_then(|v| v.pointer("/result/agents").cloned())
        .and_then(|a| a.as_array().cloned())
        .unwrap_or_default();
    s.set_status(|pane| {
        statuses
            .iter()
            .find(|a| a.get("pane_id").and_then(Value::as_str) == Some(pane))
            .and_then(|a| a.get("agent_status")?.as_str().map(String::from))
    });
    situation::render_cli(&s)
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
    if args == ["situation"] {
        return Ok(situation_cli(env));
    }
    let mode = match args {
        [h, m] if h == "hook" => m.as_str(),
        _ => {
            return Err(CliError::new(
                2,
                "usage: quaqua memory hook pre|batch|session-start | situation",
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
                "usage: quaqua memory hook pre|batch|session-start | situation",
            ))
        }
    };
    // Hooks never fail the turn: no decision is an empty answer.
    Ok(out.unwrap_or_default())
}

/// The hook's JSON: the standing block, then the re-read hint after a
/// compaction, as SessionStart `additionalContext`. `None` when both are empty.
fn standing_output(block: String, hint: Option<String>) -> Option<String> {
    let mut text = block;
    if let Some(h) = hint {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&h);
    }
    (!text.is_empty()).then(|| {
        json!({"hookSpecificOutput": {
            "hookEventName": "SessionStart", "additionalContext": text}})
        .to_string()
    })
}

/// True when the session transcript already holds a standing block. A missing
/// or unreadable path counts as "no": emit rather than leave the session bare.
fn transcript_has_block(v: &Value) -> bool {
    v.get("transcript_path")
        .and_then(Value::as_str)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|t| t.contains(STANDING_HEADER))
}

/// The SessionStart hook. Records the session's rule baseline (a `resume`
/// keeps the old one), emits the standing block (plus the shorter re-read list
/// after a compaction) as `additionalContext` for a CoS workspace session, and
/// re-tags the CoS pane when the session runs in the workspace itself.
/// A `resume` gets the block only when its transcript lacks one (a session
/// begun before this engine): Claude Code drops an identical re-emit, but an
/// edited block would stack beside the stale copy (probe, 2026-11-18).
/// Idempotent: a repeat rewrites the same baseline and finds the pane tagged.
pub fn session_start(
    ws_root: &Path,
    input: &str,
    panes: &dyn PaneOps,
    pane: Option<&str>,
) -> Option<String> {
    let v: Value = serde_json::from_str(input).unwrap_or_default();
    let sid = v.get("session_id").and_then(Value::as_str).unwrap_or("");
    let source = v.get("source").and_then(Value::as_str).unwrap_or("");
    let cwd = v
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let wants_block = !source.is_empty()
        && (source != "resume" || !transcript_has_block(&v))
        && identity::is_eligible_workspace(&cwd, ws_root);
    let block = if wants_block {
        render_standing(ws_root)
    } else {
        String::new()
    };
    let hint = record_session(ws_root, sid, source)
        .map(|full| if block.is_empty() { full } else { compact_hint(ws_root, true) });
    let text = standing_output(block, hint);
    if let Some(Err(e)) = identity::session_retag(panes, pane, &cwd, ws_root) {
        eprintln!("quaqua: CoS pane re-tag failed: {e}");
    }
    text
}
