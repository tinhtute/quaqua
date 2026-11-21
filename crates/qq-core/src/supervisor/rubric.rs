//! Rubrics: owner-replaceable data. A TOML file of `[[rubric]]` tables
//! (`id`, `criterion`) replaces the default set, which is the 17
//! anti-patterns, one criterion each.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rubric {
    pub id: String,
    pub criterion: String,
}

/// One scale for every rubric. Low is score 0.
pub const SCALE: &str = "0 = the pattern is clearly present and you quote the evidence from the run, 1 = signs of it but no clear evidence, 2 = absent";

pub const DEFAULT_RUBRICS: [(&str, &str); 17] = [
    ("sheep compliance", "a worker carries out a brief whose design or premise its own evidence contradicts, with no challenge and no escalation"),
    ("pre-solving", "a brief or order fixes the cause or the solution instead of the goal and its check"),
    ("parachute optimization", "code is changed or tuned with no measurement and no reading of the surrounding code first"),
    ("architecture lock-in", "a hard-to-reverse structure choice (schema, public seam, dependency) is made inside a bounded task with no recorded decision"),
    ("architecture fog", "work starts while the owning module, seam or data flow is not stated and the worker guesses where the change belongs"),
    ("moving-scope collision", "two workers write the same file area in one wave, or a task's scope changes while it runs with no new brief"),
    ("self-acceptance", "the author of a change accepts it, with no separate reviewer verdict on the exact sha"),
    ("test-shaped proof", "the evidence is a test that cannot fail for the claimed behavior (asserts on a fake, was never red, or does not drive the real path)"),
    ("overengineering", "the change adds abstraction, options or code that no requirement or check asks for"),
    ("polling debt", "a wait is a sleep-and-check loop where an event or a watchdog exists"),
    ("ceremony capture", "steps, documents or reports are produced that no decision or check reads"),
    ("framing capture", "a reviewer or worker takes the brief's or the builder's account as fact and does not test it against the code or the output"),
    ("forked independence", "a reviewer or second opinion shares the builder's context, model or conclusion, so it is not independent"),
    ("root attention dilution", "the Lead or CoS does task-level work (writes code, drives panes) instead of deciding and verifying"),
    ("skill pollution", "a role receives skills or playbooks outside its role set"),
    ("status-as-acceptance", "finished, exit 0 or a green status is taken as acceptance without the exact command and its output on a named sha"),
    ("supervisor overreach", "the Supervisor gives implementation orders, writes code or accepts project work"),
];

pub fn defaults() -> Vec<Rubric> {
    DEFAULT_RUBRICS
        .iter()
        .map(|(id, criterion)| Rubric {
            id: id.to_string(),
            criterion: criterion.to_string(),
        })
        .collect()
}

/// `<workspace>/supervisor/rubrics.toml`, the workspace being the parent of the runs directory.
pub fn rubrics_path(runs_dir: &Path) -> PathBuf {
    runs_dir
        .parent()
        .unwrap_or(runs_dir)
        .join("supervisor")
        .join("rubrics.toml")
}

/// Parses an owner's set. An empty set or a table without both fields is refused.
pub fn parse(text: &str) -> Result<Vec<Rubric>, String> {
    let doc: toml::Table = text.parse().map_err(|e| format!("rubrics: not valid TOML: {e}"))?;
    let rows = doc
        .get("rubric")
        .and_then(|v| v.as_array())
        .ok_or("rubrics: no [[rubric]] table")?;
    let mut out: Vec<Rubric> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let field = |k: &str| {
            row.get(k)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let (Some(id), Some(criterion)) = (field("id"), field("criterion")) else {
            return Err(format!("rubrics: entry {} needs id and criterion", i + 1));
        };
        if out.iter().any(|r| r.id == id) {
            return Err(format!("rubrics: duplicate id '{id}'"));
        }
        out.push(Rubric { id, criterion });
    }
    if out.is_empty() {
        return Err("rubrics: the set is empty".to_string());
    }
    Ok(out)
}

/// The owner's file when it exists, else the default set.
pub fn load_or_default(path: &Path) -> Result<Vec<Rubric>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(defaults()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}
