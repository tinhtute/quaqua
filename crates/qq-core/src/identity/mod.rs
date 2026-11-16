//! `qq identity heal` and the CoS pane re-tag that `memory hook session-start`
//! repeats: both restore the herdr pane tokens `grid_role=cos grid_owner=cos
//! grid_farm=<farm>` after a herdr restart dropped them. Every step reads the
//! pane first and writes only the difference, so a repeat is a no-op.
//!
//! Pane access goes through [`PaneOps`], two calls the CLI forwards to
//! `CliHerdr` (`pane_get` and `pane_report_metadata`).

pub mod peer;

use std::path::{Component, Path};

use serde_json::Value;

use crate::cli::{CliError, Command, Env, Install, Registry};
use crate::daemon::Daemon;
use crate::herdr::{CliHerdr, HerdrClient, Reply};

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

impl PaneOps for CliHerdr {
    fn pane_get(&self, pane: &str) -> Result<Value, String> {
        reply_json(HerdrClient::pane_get(self, pane))
    }
    fn pane_report_metadata(
        &self,
        pane: &str,
        source: &str,
        tokens: &[(&str, &str)],
    ) -> Result<(), String> {
        reply_json(HerdrClient::pane_report_metadata(
            self, pane, source, tokens, None,
        ))
        .map(|_| ())
    }
}

fn reply_json(r: std::io::Result<Reply>) -> Result<Value, String> {
    let r = r.map_err(|e| e.to_string())?;
    if !r.ok {
        return Err(r.error_code().unwrap_or(r.body));
    }
    serde_json::from_str(&r.body).map_err(|e| e.to_string())
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

/// Old `gate::is_eligible_workspace`: never a worker worktree, and `cwd` is the
/// workspace itself (marked by `projects/INDEX.md`) or a CoS directory with
/// its own `identity.md` / `projects/INDEX.md` (also under `.cos/`).
pub fn is_eligible_workspace(cwd: &Path, ws_root: &Path) -> bool {
    if is_worker_worktree(cwd) || is_worker_worktree(ws_root) {
        return false;
    }
    let same = match (cwd.canonicalize(), ws_root.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => cwd == ws_root,
    };
    if same && ws_root.join("projects/INDEX.md").is_file() {
        return true;
    }
    cwd.join("identity.md").is_file()
        || cwd.join(".cos/identity.md").is_file()
        || cwd.join("projects/INDEX.md").is_file()
        || cwd.join(".cos/projects/INDEX.md").is_file()
}

/// The pane this process runs in: `$HERDR_PANE`, then `$HERDR_PANE_ID`.
pub fn pane_from_env() -> Option<String> {
    ["HERDR_PANE", "HERDR_PANE_ID"].iter().find_map(|k| {
        std::env::var(k)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// The single place the CLI picks its pane client.
pub fn cli_panes() -> Box<dyn PaneOps> {
    Box::new(CliHerdr::new())
}

fn agent_session(got: &Value) -> Option<String> {
    got.pointer("/result/pane/agent_session/value")
        .or_else(|| got.pointer("/result/pane/agent_session"))
        .and_then(Value::as_str)
        .map(String::from)
        .filter(|s| !s.is_empty())
}

/// Old `record_cos_identity_if_present`: writes `runs/cos.identity.json` as
/// `{agent_session, pane_id}` (pretty, tmp + rename, only when it differs).
/// A pane with no agent session removes a record that names this same pane and
/// leaves a record for another pane alone. Returns whether a record exists for
/// `pane` afterwards.
pub fn record_identity(panes: &dyn PaneOps, pane: &str, runs_dir: &Path) -> Result<bool, String> {
    let session = agent_session(&panes.pane_get(pane)?);
    let path = runs_dir.join(COS_IDENTITY_FILE);
    let Some(session) = session else {
        if load_cos_identity(runs_dir).is_some_and(|(_, p)| p == pane) {
            let _ = std::fs::remove_file(&path);
        }
        return Ok(false);
    };
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "agent_session": session,
        "pane_id": pane,
    }))
    .map_err(|e| e.to_string())?;
    if std::fs::read_to_string(&path).is_ok_and(|cur| cur == body) {
        return Ok(true);
    }
    std::fs::create_dir_all(runs_dir).map_err(|e| format!("{}: {e}", runs_dir.display()))?;
    let tmp = runs_dir.join(format!(".{COS_IDENTITY_FILE}.tmp"));
    std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

/// The SessionStart twin of `heal`: re-tags the pane the session runs in when
/// `cwd` is an eligible CoS workspace, and records `cos.identity.json` for it.
/// A pane carrying another role is left alone entirely. `None` when not eligible.
pub fn session_retag(
    panes: &dyn PaneOps,
    pane: Option<&str>,
    cwd: &Path,
    ws_root: &Path,
) -> Option<Result<Retag, String>> {
    let pane = pane.map(str::trim).filter(|p| !p.is_empty())?;
    if !is_eligible_workspace(cwd, ws_root) {
        return None;
    }
    Some(retag_cos(panes, pane, &farm_of(ws_root)).and_then(|r| {
        if !matches!(r, Retag::Skipped(_)) {
            record_identity(panes, pane, &ws_root.join("runs"))?;
        }
        Ok(r)
    }))
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
    if agent_session(&got).as_deref() != Some(session.as_str()) {
        return Ok(vec![]);
    }
    Ok(match retag_cos(panes, &pane, &farm)? {
        Retag::Tagged => vec![format!("tagged cos pane {pane} (farm {farm})")],
        Retag::Unchanged | Retag::Skipped(_) => vec![],
    })
}

/// The entry `cli::MODULES` lists.
pub const INSTALL: Install = install;

fn install(registry: &mut Registry, daemon: &mut Daemon) -> Result<(), String> {
    daemon.require_peer(std::sync::Arc::new(peer::HerdrPeers::new(daemon.herdr())));
    registry.register(Command {
        name: "identity",
        help: "identity heal: restore the CoS pane tokens after a herdr restart",
        run: identity_cmd,
    })
}

pub fn identity_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    if args != ["heal"] {
        return Err(CliError::new(2, "usage: qq identity heal"));
    }
    let actions = heal(cli_panes().as_ref(), &env.runs_dir).map_err(|e| CliError::new(1, e))?;
    Ok(if actions.is_empty() {
        "identity heal: nothing to heal".to_string()
    } else {
        actions.join("\n")
    })
}
