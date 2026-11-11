//! Worker stop logic (`quaqua worker stop <name>`).
//!
//! Interrupts a busy worker by sending the key sequence its harness needs —
//! never `ctrl+c` (it kills opencode). The sequence is per harness and closed:
//! a harness with no verified sequence is refused, nothing is sent.
//! No rule path is cited: the esc rule is not committed to the repo yet.

use crate::herdr::HerdrOps;
use serde_json::Value;

pub const USAGE: &str = "\
# Usage: quaqua worker stop <name>
#   Interrupts a running worker with the key sequence its harness needs:
#     opencode: esc esc (one esc only arms)   omp: esc   agy: esc
#   A harness with no verified sequence is refused — nothing is sent.
#   Never ctrl+c. Stop leaves the worker alive: use `quaqua worker cleanup` after.";

/// Keys that interrupt a busy worker of this harness, in order.
/// `None` = no verified sequence: stop refuses rather than guessing.
pub fn stop_keys(harness: &str) -> Option<&'static [&'static str]> {
    crate::harness::Harness::parse(harness)?.stop_keys()
}

/// Harness of a live agent, parsed from `herdr agent get` output
/// (`result.agent.agent`). `None` when the reply is an error (agent gone) or
/// does not carry a harness — herdr I/O failures are reported by the caller,
/// not folded into this.
pub fn harness_from_info(body: &str) -> Option<String> {
    let json = serde_json::from_str::<Value>(body).ok()?;
    let harness = json.get("result")?.get("agent")?.get("agent")?.as_str()?;
    if harness.is_empty() {
        None
    } else {
        Some(harness.to_string())
    }
}

/// `worker stop` args after the subcommand word: exactly one worker name.
pub fn parse_stop_args(args: &[String]) -> Result<String, (i32, String)> {
    let usage = || (1, format!("{USAGE}\n"));
    if args.len() != 1 {
        return Err(usage());
    }
    let name = &args[0];
    if name.is_empty()
        || name.starts_with('-')
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(usage());
    }
    Ok(name.clone())
}

pub fn stop_worker<H: HerdrOps>(name: &str, herdr: &H) -> Result<(), (i32, String)> {
    println!("{}", interrupt_worker(name, herdr)?);
    Ok(())
}

/// Send the interrupt keys and return the summary line instead of printing it,
/// so callers that own stdout (the watchdog) keep it clean.
pub fn interrupt_worker<H: HerdrOps>(name: &str, herdr: &H) -> Result<String, (i32, String)> {
    let (_ok, body) = herdr.agent_get(name).map_err(|e| {
        (
            1,
            format!("error: cannot reach herdr to inspect agent {name}: {e} — nothing was sent\n"),
        )
    })?;

    let harness = harness_from_info(&body).ok_or_else(|| {
        (
            1,
            format!("error: agent '{name}' is not running or unknown to herdr — nothing was sent\n"),
        )
    })?;

    let keys = stop_keys(&harness).ok_or_else(|| {
        (
            1,
            format!(
                "error: no verified stop sequence for harness '{harness}' (agent {name}) — nothing was sent\n"
            ),
        )
    })?;

    for key in keys {
        let out = herdr
            .agent_send_keys(name, key)
            .map_err(|e| (1, format!("error: failed to send '{key}' to {name}: {e}\n")))?;
        if !out.status.success() {
            let err_text = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err((1, format!("error: failed to send '{key}' to {name}: {err_text}\n")));
        }
    }

    Ok(format!("stopped {name} (harness={harness}, keys={})", keys.join(" ")))
}
