//! The claude driver: `--model claude-<model with dashes> --effort <effort>`.

use super::HarnessDriver;

fn model_id(model: &str) -> String {
    if model.starts_with("claude-") {
        model.to_string()
    } else {
        format!("claude-{}", model.replace('.', "-"))
    }
}

pub fn driver() -> HarnessDriver {
    HarnessDriver {
        harness: "claude".into(),
        herdr_kind: "claude".into(),
        args: ["--model", "{model}", "--effort", "{effort}"]
            .map(String::from)
            .to_vec(),
        model_id,
    }
}
