//! `qq inbox`: the mail digest of one recipient, read through the bus verbs
//! `mail.inbox` and `mail.ack`. Silent when nothing waits, so a hook that runs
//! on every prompt injects nothing. At most [`MAX_LINES`] lines unless `--all`;
//! the cursor moves to the last mail shown (never past), so an overflow is
//! delivered next time. `--peek` leaves the cursor alone.

use serde_json::{json, Value};

use crate::cli::{caller_claim, rpc_call_as, CliError, Env};

/// Digest cap, overflow line included.
const MAX_LINES: usize = 20;
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
        "usage: qq inbox [--for NAME] [--peek] [--all] [--format plain|hook-ups|hook-stop]",
    )
}

pub fn inbox_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    inbox_as(env, caller_claim(), args)
}

/// `inbox` with the caller's claim read once by the caller of this function.
pub fn inbox_as(env: &Env, claim: Option<String>, args: &[String]) -> Result<String, CliError> {
    let (mut to, mut peek, mut all, mut format) = (None, false, false, Format::Plain);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--for" => to = Some(it.next().ok_or_else(usage)?.clone()),
            "--peek" => peek = true,
            "--all" => all = true,
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
        Err(_) if hook => return Ok(String::new()),
        Err(e) => return Err(e),
    };
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
        lines.push("… more waiting: qq inbox --all".to_string());
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
    let text = lines.join("\n");
    Ok(match format {
        Format::Plain => text,
        Format::HookUps => json!({"hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit", "additionalContext": text}})
        .to_string(),
        Format::HookStop => json!({"decision": "block", "reason": "New lead mail received",
            "hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": text}})
        .to_string(),
    })
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
