//! The claude driver: `--model claude-<model with dashes> --effort <effort>`.

use super::HarnessDriver;

pub struct Claude;

fn model_id(model: &str) -> String {
    if model.starts_with("claude-") {
        model.to_string()
    } else {
        format!("claude-{}", model.replace('.', "-"))
    }
}

impl HarnessDriver for Claude {
    fn harness(&self) -> &str {
        "claude"
    }

    fn herdr_kind(&self) -> &str {
        "claude"
    }

    fn launch_args(&self, model: &str, effort: Option<&str>) -> Vec<String> {
        let mut out = vec!["--model".to_string(), model_id(model)];
        if let Some(e) = effort {
            out.extend(["--effort".to_string(), e.to_string()]);
        }
        out
    }
}
