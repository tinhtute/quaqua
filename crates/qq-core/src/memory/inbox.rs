//! `quaqua inbox`: the mail digest of one recipient, read through the bus verbs
//! `mail.inbox` and `mail.ack`. Silent when nothing waits, so a hook that runs
//! on every prompt injects nothing. At most [`MAX_LINES`] lines unless `--all`;
//! the cursor moves to the last mail shown (never past), so an overflow is
//! delivered next time. `--peek` leaves the cursor alone.
//!
//! With `--session ID`, a `hook-ups` run that cannot reach the daemon prints one
//! "daemon down" line, once per session, until the daemon answers again (a
//! marker under `runs/.sessions/` records it; a successful `mail.inbox` clears
//! it). `hook-stop` never blocks on a dead daemon.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::cli::{caller_claim, rpc_call_as, CliError, Env};

/// Digest cap, overflow line included.
const MAX_LINES: usize = 20;
/// The one line a session hears while the daemon is down.
const DAEMON_DOWN: &str = "quaqua daemon down: mail not checked until it answers";
/// What `mail.inbox` returns at most per call.
const PAGE: usize = 100;

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Plain,
    HookUps,
    HookStop,
}

fn usage() -> CliError {
    CliError::new(
        2,
        "usage: quaqua inbox [--for NAME] [--peek] [--all] [--session ID] [--format plain|hook-ups|hook-stop]",
    )
}

pub fn inbox_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    inbox_as(env, caller_claim(), args)
}

/// `inbox` with the caller's claim read once by the caller of this function.
pub fn inbox_as(env: &Env, claim: Option<String>, args: &[String]) -> Result<String, CliError> {
    let (mut to, mut peek, mut all, mut format) = (None, false, false, Format::Plain);
    let mut session: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--for" => to = Some(it.next().ok_or_else(usage)?.clone()),
            "--peek" => peek = true,
            "--all" => all = true,
            "--session" => session = Some(it.next().ok_or_else(usage)?.clone()),
            "--format" => {
                format = match it.next().ok_or_else(usage)?.as_str() {
                    "plain" => Format::Plain,
                    "hook-ups" => Format::HookUps,
                    "hook-stop" => Format::HookStop,
                    other => {
                        return Err(CliError::new(
                            2,
                            format!(
                                "unknown format '{other}': expected plain, hook-ups, or hook-stop"
                            ),
                        ))
                    }
                }
            }
            _ => return Err(usage()),
        }
    }
    let mut call = json!({ "limit": if all { PAGE } else { MAX_LINES + 1 } });
    if let Some(t) = &to {
        call["to"] = json!(t);
    }
    let hook = format != Format::Plain;
    // A hook never fails the turn: with no answer it injects nothing.
    let res = match rpc_call_as(env, claim.clone(), "mail.inbox", call) {
        Ok(r) => r,
        Err(e) if hook => {
            // Only a failure to reach the daemon is "down"; a refusal or an
            // internal error is an answer, so it clears the marker.
            if !transport_error(&e) {
                clear_down(env, session.as_deref());
                return Ok(String::new());
            }
            return Ok(daemon_down(env, session.as_deref(), format));
        }
        Err(e) => return Err(e),
    };
    clear_down(env, session.as_deref());
    let mail = res["mail"].as_array().cloned().unwrap_or_default();
    if mail.is_empty() {
        return Ok(String::new());
    }
    let shown = if all || mail.len() <= MAX_LINES {
        mail.len()
    } else {
        MAX_LINES - 1
    };
    let mut lines: Vec<String> = mail[..shown].iter().map(line).collect();
    if shown < mail.len() {
        lines.push("… more waiting: quaqua inbox --all".to_string());
    }
    if !peek {
        let upto = mail[shown - 1]["id"].as_i64().unwrap_or(0);
        let mut ack = json!({ "upto": upto });
        if let Some(t) = &to {
            ack["to"] = json!(t);
        }
        match rpc_call_as(env, claim.clone(), "mail.ack", ack) {
            Ok(_) => {}
            // Not acked: the mail is shown again next time, never lost.
            Err(_) if hook => {}
            Err(e) => return Err(e),
        }
    }
    Ok(render(format, &lines.join("\n")))
}

fn render(format: Format, text: &str) -> String {
    match format {
        Format::Plain => text.to_string(),
        Format::HookUps => json!({"hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit", "additionalContext": text}})
        .to_string(),
        Format::HookStop => json!({"decision": "block", "reason": "New lead mail received",
            "hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": text}})
        .to_string(),
    }
}

/// `runs/.sessions/<id>.inbox-down`; `None` without a session id or for one
/// that could escape the dir.
fn down_marker(env: &Env, session: Option<&str>) -> Option<PathBuf> {
    let id = session?;
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| {
        env.runs_dir
            .join(".sessions")
            .join(format!("{id}.inbox-down"))
    })
}

/// `rpc_call_as` words a failure to reach the daemon as "daemon not running"
/// or "rpc failed"; every other error is the daemon's own answer.
fn transport_error(e: &CliError) -> bool {
    e.message.starts_with("daemon not running") || e.message.starts_with("rpc failed:")
}

fn clear_down(env: &Env, session: Option<&str>) {
    if let Some(m) = down_marker(env, session) {
        let _ = std::fs::remove_file(m);
    }
}

/// The hook output when the daemon did not answer: the "daemon down" line the
/// first time in a session, then nothing until the daemon has answered again.
fn daemon_down(env: &Env, session: Option<&str>, format: Format) -> String {
    let Some(marker) = down_marker(env, session) else {
        return String::new();
    };
    if format != Format::HookUps || marker.exists() {
        return String::new();
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(&marker, "").is_err() {
        return String::new();
    }
    render(format, DAEMON_DOWN)
}

/// `#<id> <from> [<kind>] <first line of the body, 100 chars>`.
fn line(m: &Value) -> String {
    let body = m["body"].as_str().unwrap_or("");
    let first: String = body
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(100)
        .collect();
    format!(
        "#{} {} [{}] {}",
        m["id"],
        m["from"].as_str().unwrap_or("?"),
        m["kind"].as_str().unwrap_or("note"),
        first
    )
}
