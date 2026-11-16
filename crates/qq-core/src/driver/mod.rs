//! Every per-harness behaviour lives here and nowhere else: the rest of the
//! crate asks the [`Registry`] for the driver of a kind's harness and never
//! branches on a harness name.
//!
//! A driver is data: the herdr agent kind it starts, an args template with
//! `{model}` and `{effort}` slots, and how a roster model name becomes the
//! harness's model id. Readiness is herdr's `agent start` (it returns once the
//! agent is up). Stop and cost parsing arrive with T41; until then a run's
//! cost is recorded as unknown.

mod claude;

use std::collections::BTreeMap;
use std::sync::Arc;

/// What a harness needs to be launched, as data.
pub struct HarnessDriver {
    /// The harness name as config spells it (`harness.name`).
    pub harness: String,
    /// The agent kind `herdr agent start --kind` is given.
    pub herdr_kind: String,
    /// Args after herdr's `--`, with `{model}` and `{effort}` slots.
    pub args: Vec<String>,
    /// Roster model name -> the id the harness's CLI takes.
    pub model_id: fn(&str) -> String,
}

impl HarnessDriver {
    /// The launch args for `model` and `effort`. With no effort the
    /// `{effort}` slot is dropped together with the flag before it.
    pub fn launch_args(&self, model: &str, effort: Option<&str>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for a in &self.args {
            match (a.as_str(), effort) {
                ("{effort}", None) => {
                    out.pop();
                }
                ("{effort}", Some(e)) => out.push(e.to_string()),
                ("{model}", _) => out.push((self.model_id)(model)),
                _ => out.push(a.clone()),
            }
        }
        out
    }
}

/// Drivers by harness name.
#[derive(Default, Clone)]
pub struct Registry {
    by_harness: BTreeMap<String, Arc<HarnessDriver>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The drivers that exist today: claude. pi, agy and the generic driver
    /// join with T41.
    pub fn native() -> Self {
        let mut r = Self::new();
        r.register(claude::driver());
        r
    }

    /// Adds a driver; the harness name is the key, a repeat replaces.
    pub fn register(&mut self, d: HarnessDriver) {
        self.by_harness.insert(d.harness.clone(), Arc::new(d));
    }

    pub fn get(&self, harness: &str) -> Option<Arc<HarnessDriver>> {
        self.by_harness.get(harness).cloned()
    }
}
