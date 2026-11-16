//! Watchers observe the outside world and push [`Signal`]s into a workflow
//! loop, one per state change. They hold no state of their own and write
//! nothing; the loop journals what it receives.
//!
//! Polling paths (none is journaled):
//! * herdr: a blocking `agent wait` for any status other than the last seen,
//!   re-armed after a timeout; other errors back off, capped at [`BACKOFF_MAX`].
//!   The watcher ends when the agent is gone or its workflow is not running.
//! * report file: an existence check every [`REPORT_POLL`] until it appears.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use super::supervisor::Supervisor;
use crate::herdr::HerdrClient;

pub const REPORT_POLL: Duration = Duration::from_millis(200);
const WAIT_TIMEOUT_MS: u64 = 30_000;
const BACKOFF: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
const STATUSES: [&str; 4] = ["idle", "working", "blocked", "done"];

fn agent_status(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.pointer("/result/agent/agent_status")?
        .as_str()
        .map(str::to_string)
}

fn nap(stop: &AtomicBool, total: Duration) {
    let step = Duration::from_millis(20);
    let mut left = total;
    while !stop.load(Ordering::SeqCst) && !left.is_zero() {
        let d = step.min(left);
        std::thread::sleep(d);
        left -= d;
    }
}

/// Emits `herdr`/`status` once for the current status and again on each change.
pub fn watch_agent(
    herdr: Arc<dyn HerdrClient>,
    agent: String,
    workflow: String,
    sup: Arc<Supervisor>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut last: Option<String> = None;
        let mut delay = BACKOFF;
        while !stop.load(Ordering::SeqCst) && sup.is_running(&workflow) {
            let reply = match &last {
                None => herdr.agent_get(&agent),
                Some(seen) => {
                    let until: Vec<&str> = STATUSES.iter().copied().filter(|s| s != seen).collect();
                    herdr.agent_wait(&agent, &until, WAIT_TIMEOUT_MS)
                }
            };
            let reply = match reply {
                Ok(r) => r,
                Err(_) => {
                    nap(&stop, delay);
                    delay = (delay * 2).min(BACKOFF_MAX);
                    continue;
                }
            };
            if !reply.ok {
                match reply.error_code().as_deref() {
                    // The agent no longer exists: nothing left to watch.
                    Some("agent_not_found") => return,
                    // A wait that ran out is the normal re-arm, not a fault.
                    Some("timeout") => nap(&stop, BACKOFF),
                    _ => {
                        nap(&stop, delay);
                        delay = (delay * 2).min(BACKOFF_MAX);
                    }
                }
                continue;
            }
            delay = BACKOFF;
            match agent_status(&reply.body) {
                Some(s) if last.as_deref() != Some(&s) => {
                    let payload = json!({ "agent": agent, "status": s });
                    if sup.signal(&workflow, "herdr", "status", payload).is_err() {
                        return;
                    }
                    last = Some(s);
                }
                _ => nap(&stop, BACKOFF),
            }
        }
    })
}

/// Emits `report`/`appeared` once when `path` exists. Reports are published
/// by rename, so existence means the content is complete.
pub fn watch_report(
    path: PathBuf,
    workflow: String,
    sup: Arc<Supervisor>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) && sup.is_running(&workflow) {
            if let Ok(meta) = std::fs::metadata(&path) {
                let payload = json!({ "path": path, "bytes": meta.len() });
                let _ = sup.signal(&workflow, "report", "appeared", payload);
                return;
            }
            nap(&stop, REPORT_POLL);
        }
    })
}
