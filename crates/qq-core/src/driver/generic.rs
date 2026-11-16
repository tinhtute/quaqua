//! The generic driver: any herdr agent kind, from config alone, degraded.

use super::HarnessDriver;
use crate::config::Driver;

pub struct Generic {
    name: String,
}

impl Generic {
    pub fn new(name: &str) -> Self {
        Generic {
            name: name.to_string(),
        }
    }
}

impl HarnessDriver for Generic {
    fn harness(&self) -> &str {
        &self.name
    }

    /// The herdr agent kind is the harness name.
    fn herdr_kind(&self) -> &str {
        &self.name
    }

    /// Config cannot say how a foreign CLI spells model or effort.
    fn launch_args(&self, _model: &str, _effort: Option<&str>) -> Vec<String> {
        Vec::new()
    }

    fn driver(&self) -> Driver {
        Driver::Generic
    }
}
