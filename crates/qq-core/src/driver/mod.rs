//! Every per-harness behaviour lives here and nowhere else: the rest of the
//! crate asks the [`Registry`] for the driver of a kind's harness and never
//! branches on a harness name.
//!
//! A driver names the herdr agent kind it starts and builds the launch args
//! (model id and effort in the harness's own spelling). Readiness is herdr's `agent start` (it returns once the
//! agent is up). A driver also owns its stop key sequences (in fallback
//! order), its native fence hook, and where a run's cost is read from.
//!
//! Degraded mode: the generic driver is built from config alone (a harness
//! row with driver `generic`). It has no hook (so it is [`Fence::Unfenced`]),
//! no cost source, no launch args, and stops only through `herdr agent stop`.

mod claude;
mod generic;
mod pi;

use crate::authority::{fence_for, Fence};
use crate::config::{Driver, Harness};
use crate::herdr::{HerdrClient, Reply, PANE_BUSY};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Token and cost totals of one run, read from a harness's own session file.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
}

/// Where a driver's fence hook was written (idempotent: same bytes, same path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub path: PathBuf,
}

/// `herdr agent start` failed in a way the caller must see.
#[derive(Debug, PartialEq, Eq)]
pub enum StartError {
    /// The pane hosts another agent. Never retried and never answered by
    /// starting a different harness: the caller decides.
    PaneBusy,
    /// herdr refused with this error code.
    Refused(String),
    /// The herdr binary could not be reached after every attempt.
    Transport(String),
}

/// Everything the engine asks of one harness. Implementations are small and
/// stateless; behaviour that differs per harness lives only in them.
pub trait HarnessDriver: Send + Sync {
    /// The harness name as config spells it (`harness.name`).
    fn harness(&self) -> &str;
    /// The agent kind `herdr agent start --kind` is given.
    fn herdr_kind(&self) -> &str;
    /// Args after herdr's `--` for a roster `model` and optional `effort`.
    fn launch_args(&self, model: &str, effort: Option<&str>) -> Vec<String>;

    /// How this driver's agent was declared in config.
    fn driver(&self) -> Driver {
        Driver::Native
    }

    /// Whether a native hook fences this driver (R11), per [`fence_for`].
    fn fence(&self) -> Fence {
        fence_for(self.driver())
    }

    /// Key sequences that stop a working agent, tried in order until it
    /// idles. Each entry is one `herdr agent send-keys` call's keys. Empty:
    /// the only stop is `herdr agent stop`.
    fn stop_sequences(&self) -> Vec<Vec<String>> {
        Vec::new()
    }

    /// Writes this driver's fence hook into `worktree`, gating through the
    /// executable `gate` (`quaqua gate` or a wrapper). `None`: no hook exists.
    fn install_hook(&self, _worktree: &Path, _gate: &Path) -> io::Result<Option<Hook>> {
        Ok(None)
    }

    /// Totals read from the harness's session file (`agent_session.value`).
    /// `None`: cost is unknown for this harness.
    fn usage(&self, _session_file: &Path) -> io::Result<Option<Usage>> {
        Ok(None)
    }
}

/// Stops a working agent: sends each of the driver's stop sequences in order,
/// waiting `settle_ms` for idle after each, and stops at the first that works.
/// Already idle converges with no keys sent. `false`: no sequence idled it
/// (the caller falls back to `herdr agent stop`).
pub fn stop(
    client: &dyn HerdrClient,
    driver: &dyn HarnessDriver,
    name: &str,
    settle_ms: u64,
) -> io::Result<bool> {
    let idle = |c: &dyn HerdrClient| -> io::Result<bool> {
        Ok(c.agent_wait(name, &["idle"], settle_ms)?.ok)
    };
    if let Ok(r) = client.agent_get(name) {
        if r.ok && r.body.contains(r#""agent_status":"idle""#) {
            return Ok(true);
        }
    }
    for seq in driver.stop_sequences() {
        let keys: Vec<&str> = seq.iter().map(String::as_str).collect();
        if !client.agent_send_keys(name, &keys)?.ok {
            continue;
        }
        if idle(client)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Starts `driver`'s agent. A transport error is retried (start converges on
/// a same-name, same-pane agent, so a repeat is safe); a busy pane is
/// reported as is, and no other driver is ever tried.
pub fn start(
    client: &dyn HerdrClient,
    driver: &dyn HarnessDriver,
    name: &str,
    pane: &str,
    timeout_ms: u32,
    model: &str,
    effort: Option<&str>,
) -> Result<Reply, StartError> {
    const ATTEMPTS: u32 = 3;
    let args = driver.launch_args(model, effort);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut last = String::new();
    for _ in 0..ATTEMPTS {
        match client.agent_start(name, driver.herdr_kind(), pane, timeout_ms, &args) {
            Ok(r) if r.ok => return Ok(r),
            Ok(r) => {
                return Err(match r.error_code() {
                    Some(c) if c == PANE_BUSY => StartError::PaneBusy,
                    Some(c) => StartError::Refused(c),
                    None => StartError::Refused(r.body),
                })
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(StartError::Transport(last))
}

/// Drivers by harness name.
#[derive(Default, Clone)]
pub struct Registry {
    by_harness: BTreeMap<String, Arc<dyn HarnessDriver>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The native drivers that exist today: claude and pi (agy is not yet
    /// measured, T39).
    pub fn native() -> Self {
        let mut r = Self::new();
        r.register(claude::Claude);
        r.register(pi::Pi);
        r
    }

    /// Natives plus a generic driver for every config harness with driver
    /// `generic` whose name no native driver already owns.
    pub fn from_config(harnesses: &[Harness]) -> Self {
        let mut r = Self::native();
        for h in harnesses {
            if h.driver == Driver::Generic && r.get(&h.name).is_none() {
                r.register(generic::Generic::new(&h.name));
            }
        }
        r
    }

    /// Adds a driver; the harness name is the key, a repeat replaces.
    pub fn register(&mut self, d: impl HarnessDriver + 'static) {
        self.by_harness.insert(d.harness().to_string(), Arc::new(d));
    }

    pub fn get(&self, harness: &str) -> Option<Arc<dyn HarnessDriver>> {
        self.by_harness.get(harness).cloned()
    }
}
