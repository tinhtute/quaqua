//! Migration from team.toml schema_version 2 to 3 (`quaqua config migrate`).
//!
//! Performs line-by-line rewrite preserving comments and formatting:
//! - Upgrades `schema_version = 2` to `3`.
//! - Inserts `[layout] use = "slp"` when existing roles match the SLP built-in.
//! - Otherwise moves `[roles.*]` tables to `[layouts.custom.roles.*]` with `[layouts.custom] depth = ...`,
//!   translating `max_peers` to `max_children` and preserving taskforce target synthesis.
//! - Refuses `schema_version > 3` with an actionable hint.
//! - No-op on already-v3 files with exit 0 ("already at schema_version 3").

use crate::config::{Config, DEFAULT_LEAD_MAX_PEERS};
use crate::layout::builtin;
use crate::layout::model::Layout;
use std::path::{Path, PathBuf};

/// Outcome of the migration analysis on a team.toml string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateResult {
    /// File was already schema_version 3; no changes needed.
    AlreadyV3,
    /// Successfully rewritten content for schema_version 3.
    Migrated(String),
}

/// Pure migration function: parses and rewrites `input` TOML text to schema_version 3.
pub fn migrate_team_toml(input: &str) -> Result<MigrateResult, String> {
    // 1. Detect schema_version from text
    let mut detected_version: Option<u64> = None;
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("schema_version") {
            let rest = rest.trim_start();
            if let Some(val_str) = rest.strip_prefix('=') {
                let val_str = val_str.trim();
                let num_str: String = val_str.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(v) = num_str.parse::<u64>() {
                    detected_version = Some(v);
                    break;
                }
            }
        }
    }

    let version = detected_version.unwrap_or(2);
    if version > 3 {
        return Err(format!(
            "schema_version {version} is newer than 3: downgrade not supported (hint: upgrade quaqua binary)"
        ));
    }
    if version == 3 {
        return Ok(MigrateResult::AlreadyV3);
    }
    if version < 2 {
        return Err(format!(
            "schema_version {version} is below 2: cannot migrate"
        ));
    }

    // 2. Parse as v2 Config to inspect role topology
    let cfg: Config =
        toml::from_str(input).map_err(|e| format!("cannot parse team.toml as Config: {e}"))?;

    let legacy = Layout::from_legacy(&cfg);
    let slp = builtin::get("slp").ok_or_else(|| "slp built-in layout missing".to_string())?;
    let matches_slp = legacy.edge_set() == slp.edge_set();

    // 3. Line-by-line rewrite preserving comments and formatting
    let mut out_lines = Vec::new();
    let lines: Vec<&str> = input.lines().collect();

    let mut in_roles_table = false;
    let mut current_role_name = String::new();
    let mut current_role_has_class = false;
    let mut current_role_has_naming = false;
    let mut current_role_has_max_children = false;
    let mut lead_max_peers_line: Option<String> = None;
    let mut seen_roles = Vec::new();
    let flush_role_metadata = |role_name: &str,
                               has_class: bool,
                               has_naming: bool,
                               has_max_children: bool,
                               out: &mut Vec<String>,
                               config: &Config| {
        if role_name.is_empty() {
            return;
        }
        match role_name {
            "cos" => {
                if !has_class {
                    out.push("class = \"cos\"".to_string());
                }
                if !has_max_children {
                    out.push(format!(
                        "max_children = {}",
                        config.limits.max_workers_per_orchestrator
                    ));
                }
            }
            "lead" => {
                if !has_class {
                    out.push("class = \"lead\"".to_string());
                }
                if !has_naming {
                    out.push("naming = \"slug-lead\"".to_string());
                }
                if !has_max_children {
                    let peers = config
                        .roles
                        .get("lead")
                        .and_then(|r| r.max_peers)
                        .unwrap_or(DEFAULT_LEAD_MAX_PEERS);
                    out.push(format!("max_children = {peers}"));
                }
            }
            "peer" => {
                if !has_class {
                    out.push("class = \"peer\"".to_string());
                }
            }
            other => {
                if !has_class {
                    out.push(format!("class = \"{other}\""));
                }
            }
        }
    };

    for line in lines.iter() {
        let trimmed = line.trim();

        // Detect schema_version line
        if trimmed.starts_with("schema_version") {
            if let Some(eq_pos) = line.find('=') {
                let prefix = &line[..eq_pos];
                let after = &line[eq_pos + 1..];
                // Keep any trailing comment on the schema_version line
                let comment_pos = after.find('#');
                let comment_suffix = match comment_pos {
                    Some(cp) => &after[cp..],
                    None => "",
                };

                if matches_slp {
                    out_lines.push(format!("{prefix}= 3{comment_suffix}"));
                    out_lines.push(String::new());
                    out_lines.push("[layout]".to_string());
                    out_lines.push("use = \"slp\"".to_string());
                } else {
                    out_lines.push(format!("{prefix}= 3{comment_suffix}"));
                    out_lines.push(String::new());
                    out_lines.push("[layout]".to_string());
                    out_lines.push("use = \"custom\"".to_string());
                    out_lines.push(String::new());
                    out_lines.push("[layouts.custom]".to_string());
                    out_lines.push(format!("depth = {}", cfg.limits.depth.max(1)));
                }
                continue;
            }
        }

        // For non-SLP roles: move [roles.<name>] to [layouts.custom.roles.<name>]
        if !matches_slp {
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                // If exiting a previous role table, flush missing metadata fields
                if in_roles_table {
                    flush_role_metadata(
                        &current_role_name,
                        current_role_has_class,
                        current_role_has_naming,
                        current_role_has_max_children,
                        &mut out_lines,
                        &cfg,
                    );
                }

                if let Some(inner) = trimmed
                    .strip_prefix("[roles.")
                    .and_then(|s| s.strip_suffix(']'))
                {
                    in_roles_table = true;
                    current_role_has_class = false;
                    current_role_has_naming = false;
                    current_role_has_max_children = false;
                    current_role_name = inner.trim().to_string();
                    seen_roles.push(current_role_name.clone());
                    let rewritten = line.replace("[roles.", "[layouts.custom.roles.");
                    out_lines.push(rewritten);
                    continue;
                } else {
                    in_roles_table = false;
                    current_role_name.clear();
                }
            } else if in_roles_table {
                if trimmed.starts_with("class") && trimmed.contains('=') {
                    current_role_has_class = true;
                } else if trimmed.starts_with("naming") && trimmed.contains('=') {
                    current_role_has_naming = true;
                } else if trimmed.starts_with("max_children") && trimmed.contains('=') {
                    current_role_has_max_children = true;
                } else if trimmed.starts_with("max_peers") && trimmed.contains('=') {
                    lead_max_peers_line = Some(line.to_string());
                    // Translate max_peers -> max_children in custom layout role
                    let rewritten = line.replace("max_peers", "max_children");
                    out_lines.push(rewritten);
                    current_role_has_max_children = true;
                    continue;
                }
            }
        }

        out_lines.push((*line).to_string());
    }

    // Flush last role metadata if file ended inside a role table
    if !matches_slp && in_roles_table {
        flush_role_metadata(
            &current_role_name,
            current_role_has_class,
            current_role_has_naming,
            current_role_has_max_children,
            &mut out_lines,
            &cfg,
        );
    }

    // For non-SLP migration, synthesize taskforce role only when some v2 role spawns taskforce
    let any_role_spawns_taskforce = cfg
        .roles
        .values()
        .any(|r| r.can_spawn.iter().any(|s| s == "taskforce"));
    if !matches_slp && any_role_spawns_taskforce && !seen_roles.iter().any(|r| r == "taskforce") {
        out_lines.push(String::new());
        out_lines.push("[layouts.custom.roles.taskforce]".to_string());
        out_lines.push("class = \"taskforce\"".to_string());
        out_lines.push("can_spawn = []".to_string());
        out_lines.push("can_prompt = []".to_string());
        out_lines.push("reports_to = \"owner\"".to_string());

        let mut carried_members = Vec::new();
        if let Some(tf) = cfg.taskforce.get("taskforce") {
            carried_members.extend(tf.members.clone());
        } else {
            for tf in cfg.taskforce.values() {
                for m in &tf.members {
                    if !carried_members.contains(m) {
                        carried_members.push(m.clone());
                    }
                }
            }
        }
        if carried_members.len() > 5 {
            return Err(format!(
                "cannot migrate taskforce members: carried member count {} exceeds maximum allowed of 5",
                carried_members.len()
            ));
        }
        if !carried_members.is_empty() {
            let formatted: Vec<String> = carried_members
                .iter()
                .map(|m| format!("{{ kind = \"{m}\" }}"))
                .collect();
            out_lines.push(format!("members = [{}]", formatted.join(", ")));
        }
    }

    // Keep max_peers in [roles.lead] so cap::owner_cap continues to read it
    if !matches_slp {
        if let Some(peers) = cfg.roles.get("lead").and_then(|r| r.max_peers) {
            out_lines.push(String::new());
            out_lines.push("[roles.lead]".to_string());
            let line_to_use = lead_max_peers_line.unwrap_or_else(|| format!("max_peers = {peers}"));
            out_lines.push(line_to_use);
        }
    }

    let mut result = out_lines.join("\n");
    if input.ends_with('\n') {
        result.push('\n');
    }

    // 4. Validate rewritten TOML against Config parser and Layout validator
    let migrated_cfg: Config =
        toml::from_str(&result).map_err(|e| format!("migrated TOML failed parse check: {e}"))?;
    crate::config::validate(&migrated_cfg)
        .map_err(|e| format!("migrated TOML failed validation check: {e}"))?;

    for custom_layout in migrated_cfg.layouts.values() {
        crate::layout::validate_layout_with_config(custom_layout, &migrated_cfg)
            .map_err(|e| format!("migrated layout failed validation check: {e}"))?;
    }

    Ok(MigrateResult::Migrated(result))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOp<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

/// Compute a unified diff between old and new text with standard `@@` hunks.
pub fn format_diff(old_text: &str, new_text: &str, file_name: &str) -> String {
    if old_text == new_text {
        return String::new();
    }

    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let n = old_lines.len();
    let m = new_lines.len();

    // Standard LCS dynamic programming table
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            if old_lines[i] == new_lines[j] {
                dp[i][j] = dp[i + 1][j + 1] + 1;
            } else {
                dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    let mut ops = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < n || j < m {
        if i < n && j < m && old_lines[i] == new_lines[j] {
            ops.push(DiffOp::Equal(old_lines[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || dp[i + 1][j] >= dp[i][j + 1]) {
            ops.push(DiffOp::Delete(old_lines[i]));
            i += 1;
        } else {
            ops.push(DiffOp::Insert(new_lines[j]));
            j += 1;
        }
    }

    // Collect changed positions
    let mut change_indices = Vec::new();
    for (idx, op) in ops.iter().enumerate() {
        if !matches!(op, DiffOp::Equal(_)) {
            change_indices.push(idx);
        }
    }

    if change_indices.is_empty() {
        return String::new();
    }

    const CONTEXT: usize = 3;
    let mut hunk_ranges: Vec<(usize, usize)> = Vec::new();
    for &ch in &change_indices {
        let start = ch.saturating_sub(CONTEXT);
        let end = (ch + CONTEXT + 1).min(ops.len());
        if let Some(last) = hunk_ranges.last_mut() {
            if start <= last.1 {
                last.1 = end.max(last.1);
                continue;
            }
        }
        hunk_ranges.push((start, end));
    }

    let mut out = String::new();
    out.push_str(&format!("--- {file_name}\n+++ {file_name}\n"));

    let mut cur_old_line = 1usize;
    let mut cur_new_line = 1usize;
    let mut op_idx = 0usize;

    for (hunk_start, hunk_end) in hunk_ranges {
        // Advance line numbers up to hunk_start
        while op_idx < hunk_start {
            match ops[op_idx] {
                DiffOp::Equal(_) => {
                    cur_old_line += 1;
                    cur_new_line += 1;
                }
                DiffOp::Delete(_) => {
                    cur_old_line += 1;
                }
                DiffOp::Insert(_) => {
                    cur_new_line += 1;
                }
            }
            op_idx += 1;
        }

        let hunk_old_start = cur_old_line;
        let hunk_new_start = cur_new_line;
        let mut hunk_old_count = 0usize;
        let mut hunk_new_count = 0usize;

        for op in &ops[hunk_start..hunk_end] {
            match op {
                DiffOp::Equal(_) => {
                    hunk_old_count += 1;
                    hunk_new_count += 1;
                }
                DiffOp::Delete(_) => {
                    hunk_old_count += 1;
                }
                DiffOp::Insert(_) => {
                    hunk_new_count += 1;
                }
            }
        }

        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk_old_start, hunk_old_count, hunk_new_start, hunk_new_count
        ));

        while op_idx < hunk_end {
            match ops[op_idx] {
                DiffOp::Equal(line) => {
                    out.push_str(&format!(" {line}\n"));
                    cur_old_line += 1;
                    cur_new_line += 1;
                }
                DiffOp::Delete(line) => {
                    out.push_str(&format!("-{line}\n"));
                    cur_old_line += 1;
                }
                DiffOp::Insert(line) => {
                    out.push_str(&format!("+{line}\n"));
                    cur_new_line += 1;
                }
            }
            op_idx += 1;
        }
    }

    out
}

/// CLI runner for `quaqua config migrate`.
///
/// - Defaults to dry-run diff printed to stdout.
/// - If `write == true`, saves `<path>.bak` then writes new content via `state::atomic_write`.
/// - If file is already v3, prints "already at schema_version 3" and returns Ok.
pub fn run_migrate(path_override: Option<&Path>, write: bool) -> Result<String, String> {
    let target_path = path_override
        .map(PathBuf::from)
        .or_else(crate::config::team_config_path)
        .unwrap_or_else(|| PathBuf::from("team.toml"));

    if !target_path.is_file() {
        return Err(format!(
            "cannot read team configuration at {}",
            target_path.display()
        ));
    }

    let content = std::fs::read_to_string(&target_path)
        .map_err(|e| format!("cannot read {}: {e}", target_path.display()))?;

    match migrate_team_toml(&content)? {
        MigrateResult::AlreadyV3 => Ok("already at schema_version 3\n".to_string()),
        MigrateResult::Migrated(new_content) => {
            if write {
                let bak_path = target_path.with_extension("toml.bak");
                std::fs::write(&bak_path, content.as_bytes())
                    .map_err(|e| format!("failed to write backup {}: {e}", bak_path.display()))?;
                crate::state::atomic_write(&target_path, new_content.as_bytes())
                    .map_err(|e| format!("failed to write {}: {e}", target_path.display()))?;
                Ok(format!(
                    "migrated {} to schema_version 3 (backup at {})\n",
                    target_path.display(),
                    bak_path.display()
                ))
            } else {
                let fname = target_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("team.toml");
                Ok(format_diff(&content, &new_content, fname))
            }
        }
    }
}
