//! Watchers observe the outside world and push [`Signal`]s into a workflow
//! loop, one per state change. They hold no state of their own and write
//! nothing; the loop journals what it receives.
//!
//! Polling paths (none is journaled):
//! * herdr: a blocking `agent wait` for any status other than the last seen,
//!   re-armed after a timeout; other errors back off, capped at [`BACKOFF_MAX`].
//!   The watcher ends when the agent is gone or its workflow is not running.
//! * report file: an existence check every [`REPORT_POLL`] until it appears.
//!
//! The state sweeper is the one watcher that holds state: it polls every open
//! Lead and Peer row and mails the CoS on a move into stalled, blocked, done or
//! exited (see [`state_sweeper`]). It signals no workflow.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::supervisor::Supervisor;
use super::Ctx;
use crate::bus::{self, AgentState};
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

/// Seconds an agent may stay `working` without a status change before it is
/// stalled. `QQ_STALL_SEC` or the setting `notify.stall_sec` overrides it.
pub const STALL_SEC: f64 = 600.0;
const SWEEP_MS: u64 = 2_000;
/// A fresh agent that herdr does not know yet is not "exited" before this.
const SPAWN_GRACE_MS: u64 = 30_000;

struct Seen {
    status: String,
    since: Instant,
    stalled: bool,
}

fn number_setting(ctx: &Ctx, key: &str) -> Option<f64> {
    ctx.store
        .with_conn(|c| crate::config::setting(c, key))
        .ok()
        .flatten()
        .and_then(|v| v.as_f64())
        .filter(|v| *v >= 0.0)
}

fn stall_after(ctx: &Ctx) -> Duration {
    let secs = std::env::var("QQ_STALL_SEC")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v >= 0.0)
        .or_else(|| number_setting(ctx, "notify.stall_sec"))
        .unwrap_or(STALL_SEC);
    Duration::from_secs_f64(secs)
}

fn sweep_every(ctx: &Ctx) -> Duration {
    Duration::from_millis(number_setting(ctx, "notify.sweep_ms").map_or(SWEEP_MS, |v| v as u64))
}

/// `(name, role, started_ms)` of every open Lead and Peer.
fn open_agents(ctx: &Ctx) -> Vec<(String, String, u64)> {
    ctx.store
        .with_conn(|c| {
            let mut st = c.prepare(
                "SELECT name, role, started_ms FROM agent \
                 WHERE ended_ms IS NULL AND role IN ('lead', 'peer')",
            )?;
            let rows = st.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64))
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
}

fn is_open(ctx: &Ctx, name: &str) -> bool {
    ctx.store
        .with_conn(|c| {
            c.query_row(
                "SELECT 1 FROM agent WHERE name = ?1 AND ended_ms IS NULL",
                [name],
                |_| Ok(()),
            )
        })
        .is_ok()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Polls every open Lead and Peer in herdr and mails the CoS once per move
/// into stalled (`working` with no status change for [`STALL_SEC`]), blocked,
/// done or exited. idle and working flips are silent. An agent whose row
/// cleanup closed is dropped without a word; only a vanished agent with an open
/// row is exited. Any status change is progress and re-arms the stall clock.
///
/// Polls, with no blocking wait, so a stop is seen within one nap. Holds the
/// daemon only weakly. Memory is in-process: a restart mails a standing
/// blocked or done again once.
pub fn state_sweeper(ctx: Weak<Ctx>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut seen: HashMap<String, Seen> = HashMap::new();
        let mut gone: HashSet<String> = HashSet::new();
        while !stop.load(Ordering::SeqCst) {
            let Some(ctx) = ctx.upgrade() else { return };
            let (stall, every) = (stall_after(&ctx), sweep_every(&ctx));
            let open = open_agents(&ctx);
            let names: HashSet<&str> = open.iter().map(|(n, _, _)| n.as_str()).collect();
            seen.retain(|n, _| names.contains(n.as_str()));
            gone.retain(|n| names.contains(n.as_str()));
            for (name, role, started) in &open {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(reply) = ctx.herdr.agent_get(name) else {
                    continue;
                };
                if !reply.ok {
                    if reply.error_code().as_deref() != Some("agent_not_found")
                        || gone.contains(name)
                    {
                        continue;
                    }
                    let fresh = !seen.contains_key(name)
                        && now_ms().saturating_sub(*started) < SPAWN_GRACE_MS;
                    if !fresh && is_open(&ctx, name) {
                        gone.insert(name.clone());
                        seen.remove(name);
                        bus::state_mail(&ctx, name, role, AgentState::Exited, "");
                    }
                    continue;
                }
                let Some(status) = agent_status(&reply.body) else {
                    continue;
                };
                gone.remove(name);
                let entry = seen.get_mut(name).filter(|e| e.status == status);
                match entry {
                    Some(e) => {
                        if status == "working" && !e.stalled && e.since.elapsed() >= stall {
                            e.stalled = true;
                            let detail = format!(" (no status change for {}s)", stall.as_secs());
                            bus::state_mail(&ctx, name, role, AgentState::Stalled, &detail);
                        }
                    }
                    None => {
                        seen.insert(
                            name.clone(),
                            Seen {
                                status: status.clone(),
                                since: Instant::now(),
                                stalled: false,
                            },
                        );
                        match status.as_str() {
                            "blocked" => bus::state_mail(&ctx, name, role, AgentState::Blocked, ""),
                            "done" => bus::state_mail(&ctx, name, role, AgentState::Done, ""),
                            _ => {}
                        }
                    }
                }
            }
            drop(ctx);
            nap(&stop, every);
        }
    })
}
