//! The generic driver: any herdr agent kind, from config alone, degraded.
//! It renders the declared args template and keeps the declared stop keys and
//! error-line shape; cost, hook and folder-trust handling stay absent.

use super::{ErrorShape, HarnessDriver};
use crate::config::{Driver, HarnessSpec};

pub struct Generic {
    name: String,
    spec: Option<HarnessSpec>,
}

impl Generic {
    /// `spec: None` is a row stored before harnesses were declared: the kind
    /// is the harness name and there are no launch args.
    pub fn new(name: &str, spec: Option<HarnessSpec>) -> Self {
        Generic {
            name: name.to_string(),
            spec,
        }
    }
}

/// Renders each template element. A token naming `{effort}` is dropped when
/// there is no effort, with the plain flag just before it (`--effort`), so a
/// literal placeholder never reaches the CLI.
fn render(template: &[String], model: &str, effort: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Whether the last pushed token is a plain flag a dropped value may take with it.
    let mut flag_last = false;
    for t in template {
        if t.contains("{effort}") {
            let Some(e) = effort else {
                if flag_last {
                    out.pop();
                }
                flag_last = false;
                continue;
            };
            out.push(t.replace("{effort}", e).replace("{model}", model));
            flag_last = false;
            continue;
        }
        flag_last = t.starts_with('-') && !t.contains('{');
        out.push(t.replace("{model}", model));
    }
    out
}

impl HarnessDriver for Generic {
    fn harness(&self) -> &str {
        &self.name
    }

    /// The declared herdr agent kind; the harness name for a legacy row.
    fn herdr_kind(&self) -> &str {
        self.spec.as_ref().map_or(&self.name, |s| &s.herdr_kind)
    }

    fn launch_args(&self, model: &str, effort: Option<&str>) -> Vec<String> {
        self.spec
            .as_ref()
            .map(|s| render(&s.args, model, effort))
            .unwrap_or_default()
    }

    fn stop_sequences(&self) -> Vec<Vec<String>> {
        match &self.spec {
            Some(s) if !s.stop_keys.is_empty() => vec![s.stop_keys.clone()],
            _ => Vec::new(),
        }
    }

    fn error_shape(&self) -> Option<ErrorShape> {
        self.spec
            .as_ref()?
            .error_shape
            .as_deref()
            .and_then(ErrorShape::parse)
    }

    fn driver(&self) -> Driver {
        Driver::Generic
    }
}
