//! `qq identity heal` and the CoS pane re-tag that `memory hook session-start`
//! repeats: both restore the herdr pane tokens `grid_role=cos grid_owner=cos
//! grid_farm=<farm>` after a herdr restart dropped them. Every step reads the
//! pane first and writes only the difference, so a repeat is a no-op.
//!
//! Pane access goes through [`PaneOps`], two calls the wave-end commit wires to
//! `HerdrClient` (T44's `pane_get` and `pane_report_metadata`). Until then the
//! CLI uses [`Unwired`], which refuses at the first pane call.

use std::path::{Component, Path};

use serde_json::Value;

use crate::cli::{CliError, Command, Env, Install, Registry};
use crate::daemon::Daemon;

pub const COS_IDENTITY_FILE: &str = "cos.identity.json";
/// The `--source` herdr records the tokens under.
pub const TOKEN_SOURCE: &str = "herdr-quaqua";

/// The two herdr pane calls identity needs.
pub trait PaneOps {
    /// `herdr pane get PANE`, parsed: `{"result":{"pane":{"tokens":{..},"agent_session":..}}}`.
    fn pane_get(&self, pane: &str) -> Result<Value, String>;
    /// `herdr pane report-metadata PANE --source SOURCE --token K=V ...`; tokens merge.
    fn pane_report_metadata(
        &self,
        pane: &str,
        source: &str,
        tokens: &[(&str, &str)],
    ) -> Result<(), String>;
}

/// The CLI's pane client until `HerdrClient` has the two methods.
pub struct Unwired;

impl PaneOps for Unwired {
    fn pane_get(&self, _: &str) -> Result<Value, String> {
        Err("herdr pane calls are not wired (HerdrClient pane_get / pane_report_metadata)".into())
    }
    fn pane_report_metadata(&self, _: &str, _: &str, _: &[(&str, &str)]) -> Result<(), String> {
        Err("herdr pane calls are not wired (HerdrClient pane_get / pane_report_metadata)".into())
    }
}

/// What a re-tag did to one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retag {
    /// The pane carries another role; left alone.
    Skipped(String),
    /// Already `cos` with this farm: nothing written.
    Unchanged,
    /// Tokens written.
    Tagged,
}

/// Farm id of a workspace root: its last component, lowercased, reduced to
/// `[a-z0-9_-]`.
pub fn farm_of(root: &Path) -> String {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let last = root
        .components()
        .rev()
        .find_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .unwrap_or_default();
    last.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn token(pane: &Value, key: &str) -> Option<String> {
    pane.pointer(&format!("/result/pane/tokens/{key}"))
        .and_then(Value::as_str)
        .map(String::from)
}

/// Tags `pane` as the CoS pane of `farm`. A pane that carries a role other than
/// `cos` is never touched; one already complete is not rewritten.
pub fn retag_cos(panes: &dyn PaneOps, pane: &str, farm: &str) -> Result<Retag, String> {
    if pane.is_empty() {
        return Err("pane is empty".into());
    }
    let got = panes.pane_get(pane)?;
    match token(&got, "grid_role") {
        Some(r) if r != "cos" => return Ok(Retag::Skipped(r)),
        Some(_)
            if token(&got, "grid_owner").as_deref() == Some("cos")
                && token(&got, "grid_farm").as_deref() == Some(farm) =>
        {
            return Ok(Retag::Unchanged)
        }
        _ => {}
    }
    panes.pane_report_metadata(
        pane,
        TOKEN_SOURCE,
        &[
            ("grid_role", "cos"),
            ("grid_owner", "cos"),
            ("grid_farm", farm),
        ],
    )?;
    Ok(Retag::Tagged)
}

fn is_worker_worktree(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".wt") || path.join(".git").is_file()
}

/// The SessionStart twin of `heal`: re-tags the pane the session runs in when
/// `cwd` is the workspace itself (not a worker worktree). `pane` is
/// `$HERDR_PANE_ID`. Returns the re-tag, or `None` when not eligible.
pub fn session_retag(
    panes: &dyn PaneOps,
    pane: Option<&str>,
    cwd: &Path,
    ws_root: &Path,
) -> Option<Result<Retag, String>> {
    let pane = pane.map(str::trim).filter(|p| !p.is_empty())?;
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if is_worker_worktree(cwd) || !same(cwd, ws_root) {
        return None;
    }
    Some(retag_cos(panes, pane, &farm_of(ws_root)))
}

/// The CoS identity `runs/cos.identity.json` records: `(agent_session, pane_id)`.
fn load_cos_identity(runs_dir: &Path) -> Option<(String, String)> {
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(runs_dir.join(COS_IDENTITY_FILE)).ok()?)
            .ok()?;
    let s = v.get("agent_session")?;
    let session = s
        .get("value")
        .and_then(Value::as_str)
        .or_else(|| s.as_str())?
        .trim()
        .to_string();
    let pane = v.get("pane_id")?.as_str()?.trim().to_string();
    (!session.is_empty() && !pane.is_empty()).then_some((session, pane))
}

/// Restores the CoS pane tokens: the pane recorded in `cos.identity.json` is
/// tagged when it still hosts the recorded agent session and carries no other
/// role. Anything else is a no-op (fail closed). One line per action.
pub fn heal(panes: &dyn PaneOps, runs_dir: &Path) -> Result<Vec<String>, String> {
    let Some((session, pane)) = load_cos_identity(runs_dir) else {
        return Ok(vec![]);
    };
    let farm = runs_dir
        .parent()
        .map(farm_of)
        .ok_or_else(|| format!("{} has no workspace parent", runs_dir.display()))?;
    let got = panes.pane_get(&pane)?;
    let live = got
        .pointer("/result/pane/agent_session/value")
        .or_else(|| got.pointer("/result/pane/agent_session"))
        .and_then(Value::as_str);
    if live != Some(session.as_str()) {
        return Ok(vec![]);
    }
    Ok(match retag_cos(panes, &pane, &farm)? {
        Retag::Tagged => vec![format!("tagged cos pane {pane} (farm {farm})")],
        Retag::Unchanged | Retag::Skipped(_) => vec![],
    })
}

/// The entry `cli::MODULES` lists.
pub const INSTALL: Install = install;

fn install(registry: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "identity",
        help: "identity heal: restore the CoS pane tokens after a herdr restart",
        run: identity_cmd,
    })
}

fn identity_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    if args != ["heal"] {
        return Err(CliError::new(2, "usage: qq identity heal"));
    }
    let actions = heal(&Unwired, &env.runs_dir).map_err(|e| CliError::new(1, e))?;
    Ok(if actions.is_empty() {
        "identity heal: nothing to heal".to_string()
    } else {
        actions.join("\n")
    })
}
