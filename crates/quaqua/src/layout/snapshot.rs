//! Per-project layout snapshot and refreeze operations (`runs/<slug>.layout.toml`).
//!
//! Provides atomic creation, loading, refreezing, and owner-chain resolution for frozen project layouts.

use crate::config::Config;
use crate::layout::model::Layout;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Serialized layout topology snapshot persisted in `runs/<slug>.layout.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutSnapshot {
    /// SHA-256 hex digest of the canonical serialized `Layout` topology.
    pub sha256: String,
    /// Origin descriptor with modification timestamp, e.g. `team.toml@1729780000`.
    pub source: String,
    /// Frozen layout topology (roles, classes, allow-lists, caps).
    pub layout: Layout,
}

/// Where an effective layout originated from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutOrigin {
    /// Frozen snapshot file in `runs/<slug>.layout.toml`.
    Snapshot {
        slug: String,
        sha256: String,
        path: PathBuf,
    },
    /// Live configuration (resolved from `team.toml` or synthesized legacy).
    Live { source: &'static str },
}

impl LayoutOrigin {
    pub fn is_snapshot(&self) -> bool {
        matches!(self, LayoutOrigin::Snapshot { .. })
    }

    pub fn source_desc(&self) -> String {
        match self {
            LayoutOrigin::Snapshot { slug, path, .. } => {
                format!(
                    "snapshot {}",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or(slug)
                )
            }
            LayoutOrigin::Live { source } => source.to_string(),
        }
    }
}

/// Compute the SHA-256 digest of a Layout's canonical TOML representation.
pub fn layout_sha256(layout: &Layout) -> Result<String, String> {
    let toml_str = toml::to_string_pretty(layout)
        .map_err(|e| format!("failed to serialize layout for hashing: {e}"))?;
    Ok(crate::hash::sha256_hex(toml_str.as_bytes()))
}

/// Read the modified timestamp of the resolved `team.toml` (seconds since UNIX epoch).
pub fn team_config_mtime() -> u64 {
    let path = match crate::config::team_config_path() {
        Some(p) => p,
        None => return 0,
    };
    std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Construct the standard snapshot source tag: `team.toml@<mtime>`.
pub fn snapshot_source_tag() -> String {
    format!("team.toml@{}", team_config_mtime())
}

/// Write a resolved project layout snapshot to `runs/<slug>.layout.toml` via `state::atomic_write`.
pub fn write_layout_snapshot(runs_dir: &Path, slug: &str, cfg: &Config) -> Result<PathBuf, String> {
    let layout = crate::layout::resolve_layout(cfg)?;
    let sha256 = layout_sha256(&layout)?;
    let source = snapshot_source_tag();
    let snapshot = LayoutSnapshot {
        sha256,
        source,
        layout,
    };
    let toml_str = toml::to_string_pretty(&snapshot)
        .map_err(|e| format!("failed to serialize layout snapshot for {slug}: {e}"))?;
    let path = crate::state::WorkerMetadata::layout_path(runs_dir, slug);
    crate::state::atomic_write(&path, toml_str.as_bytes())
        .map_err(|e| format!("failed to write layout snapshot {}: {e}", path.display()))?;
    Ok(path)
}

/// Read and deserialize `runs/<slug>.layout.toml` if it exists.
pub fn read_layout_snapshot(runs_dir: &Path, slug: &str) -> Result<Option<LayoutSnapshot>, String> {
    let path = crate::state::WorkerMetadata::layout_path(runs_dir, slug);
    if !path.is_file() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read layout snapshot {}: {e}", path.display()))?;
    let snapshot: LayoutSnapshot = toml::from_str(&content)
        .map_err(|e| format!("invalid layout snapshot in {}: {e}", path.display()))?;
    Ok(Some(snapshot))
}

/// Refreeze the project snapshot in `runs/<slug>.layout.toml` from the live `team.toml`.
pub fn run_refreeze(slug: &str) -> Result<String, String> {
    let runs_dir = crate::worker::cleanup::default_runs_dir();
    let base_cfg = crate::config::load_team_config().map_err(|(_, msg)| msg.trim().to_string())?;
    let cfg = crate::config::resolve(base_cfg.clone(), Some(slug)).unwrap_or(base_cfg);
    crate::config::validate(&cfg)?;
    let path = write_layout_snapshot(&runs_dir, slug, &cfg)?;
    Ok(format!("refroze layout for '{slug}' at {}", path.display()))
}

/// Walk the owner chain up from `start` in `runs_dir` to determine if any ancestor is `<slug>-lead`.
///
/// Returns `Some(slug)` if the caller is or descends from a project Lead.
/// Returns `None` if the caller terminates at `"cos"`, has no `.owner` file, or if a cycle is detected.
pub fn find_lead_in_owner_chain(runs_dir: &Path, start: &str) -> Option<String> {
    let start_trim = start.trim();
    if start_trim.is_empty() || start_trim == "cos" {
        return None;
    }

    if let Some(slug) = start_trim.strip_suffix("-lead") {
        if !slug.is_empty() {
            return Some(slug.to_string());
        }
    }

    let mut current = start_trim.to_string();
    let mut visited = HashSet::new();

    for _ in 0..32 {
        if visited.contains(&current) {
            return None;
        }
        visited.insert(current.clone());

        let owner_path = crate::state::WorkerMetadata::owner_path(runs_dir, &current);
        let content = std::fs::read_to_string(&owner_path).ok()?;
        let parent = content.trim();
        if parent.is_empty() || parent == "cos" {
            return None;
        }

        if let Some(slug) = parent.strip_suffix("-lead") {
            if !slug.is_empty() {
                return Some(slug.to_string());
            }
        }

        current = parent.to_string();
    }

    None
}

/// The single entry point to resolve an effective Layout and its Origin for a given caller.
///
/// - If the caller's owner chain reaches `<slug>-lead` and `runs/<slug>.layout.toml` exists,
///   returns the frozen `Layout` from the snapshot (`LayoutOrigin::Snapshot`).
/// - Otherwise (CoS-owned work, or missing snapshot), resolves from the live `Config` (`LayoutOrigin::Live`).
pub fn resolve_effective_layout(
    caller_name: &str,
    runs_dir: &Path,
    cfg: &Config,
) -> Result<(Layout, LayoutOrigin), String> {
    if let Some(slug) = find_lead_in_owner_chain(runs_dir, caller_name) {
        if let Some(snapshot) = read_layout_snapshot(runs_dir, &slug)? {
            let path = crate::state::WorkerMetadata::layout_path(runs_dir, &slug);
            return Ok((
                snapshot.layout,
                LayoutOrigin::Snapshot {
                    slug,
                    sha256: snapshot.sha256,
                    path,
                },
            ));
        }
    }

    let layout = crate::layout::resolve_layout(cfg)?;
    let source = if cfg.schema_version >= 3 && cfg.layout.is_some() {
        "config"
    } else {
        "synthesized legacy"
    };
    Ok((layout, LayoutOrigin::Live { source }))
}
