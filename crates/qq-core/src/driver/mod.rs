//! Every per-harness behaviour lives here and nowhere else: the rest of the
//! crate asks the [`Registry`] for the driver of a kind's harness and never
//! branches on a harness name.
//!
//! A driver names the herdr agent kind it starts and builds the launch args
//! (model id and effort in the harness's own spelling). Readiness is herdr's `agent start` (it returns once the
//! agent is up). Stop and cost parsing arrive with T41; until then a run's
//! cost is recorded as unknown.

mod claude;

use std::collections::BTreeMap;
use std::sync::Arc;

/// Everything the engine asks of one harness. Implementations are small and
/// stateless; behaviour that differs per harness lives only in them.
pub trait HarnessDriver: Send + Sync {
    /// The harness name as config spells it (`harness.name`).
    fn harness(&self) -> &str;
    /// The agent kind `herdr agent start --kind` is given.
    fn herdr_kind(&self) -> &str;
    /// Args after herdr's `--` for a roster `model` and optional `effort`.
    fn launch_args(&self, model: &str, effort: Option<&str>) -> Vec<String>;
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

    /// The drivers that exist today: claude. pi, agy and the generic driver
    /// join with T41.
    pub fn native() -> Self {
        let mut r = Self::new();
        r.register(claude::Claude);
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
