//! KB: the zbrain read path for workers and the generated read-only code-lookup
//! config (R10). Nothing here builds or stores a code graph.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::{Install, Registry};
use crate::daemon::Daemon;

pub const INSTALL: Install = install;

fn install(_: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    Ok(())
}

/// Who calls zbrain. Only the CoS and the Supervisor jobs write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    Cos,
    SupervisorJob,
    /// The short-lived Supervisor agent: read-only by R13.
    SupervisorAgent,
    Lead,
    Peer,
}

impl Caller {
    /// Role names as the topology spells them; an unknown role is a Peer, the
    /// least privileged caller.
    pub fn from_role(role: &str) -> Caller {
        match role {
            "cos" => Caller::Cos,
            "supervisor-job" | "supervisor_job" => Caller::SupervisorJob,
            "supervisor" | "supervisor-agent" | "supervisor_agent" => Caller::SupervisorAgent,
            "lead" => Caller::Lead,
            _ => Caller::Peer,
        }
    }

    pub fn may_write_zbrain(self) -> bool {
        matches!(self, Caller::Cos | Caller::SupervisorJob)
    }
}

/// zbrain tools that never mutate: the whole worker read path.
pub const ZBRAIN_READ_TOOLS: [&str; 4] = [
    "memory_ask",
    "memory_status",
    "workspace_current",
    "campaign_next",
];

/// zbrain tools that create, change or rebuild stored knowledge.
pub const ZBRAIN_WRITE_TOOLS: [&str; 6] = [
    "campaign_begin",
    "campaign_submit_draft",
    "claim_draft",
    "claim_lifecycle",
    "evidence_capture",
    "memory_reindex",
];

const ZBRAIN_PREFIX: &str = "mcp__zbrain__";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub caller: Caller,
    pub tool: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "zbrain {} refused for {:?}: only the CoS and Supervisor jobs write to zbrain",
            self.tool, self.caller
        )
    }
}

/// Allow a zbrain call. A writer may call any zbrain tool; every other caller
/// may call only a tool on the read list, so a tool zbrain adds later is
/// refused for workers until it is classified here.
pub fn zbrain_check(caller: Caller, tool: &str) -> Result<(), Refusal> {
    let name = tool.strip_prefix(ZBRAIN_PREFIX).unwrap_or(tool);
    if caller.may_write_zbrain() || ZBRAIN_READ_TOOLS.contains(&name) {
        Ok(())
    } else {
        Err(Refusal {
            caller,
            tool: name.to_string(),
        })
    }
}

/// The zbrain tools a caller may be given, fully qualified.
pub fn zbrain_tools_for(caller: Caller) -> Vec<String> {
    let mut names: Vec<&str> = ZBRAIN_READ_TOOLS.to_vec();
    if caller.may_write_zbrain() {
        names.extend(ZBRAIN_WRITE_TOOLS);
    }
    names.into_iter().map(|n| format!("{ZBRAIN_PREFIX}{n}")).collect()
}

/// Serena's memory tools. All are excluded, reads included.
pub const SERENA_MEMORY_TOOLS: [&str; 6] = [
    "read_memory",
    "list_memories",
    "write_memory",
    "edit_memory",
    "delete_memory",
    "rename_memory",
];

/// Serena's editing tools, dropped by `read_only: true`; kept here so a test
/// can assert none is enabled.
pub const SERENA_EDIT_TOOLS: [&str; 11] = [
    "create_text_file",
    "delete_lines",
    "insert_after_symbol",
    "insert_at_line",
    "insert_before_symbol",
    "replace_content",
    "replace_in_files",
    "replace_lines",
    "replace_symbol_body",
    "rename_symbol",
    "safe_delete_symbol",
];

/// The tools the generated config leaves enabled (verified against a live
/// server start, see tests/kb.rs).
pub const CODE_LOOKUP_TOOLS: [&str; 8] = [
    "search_for_pattern",
    "get_symbols_overview",
    "find_symbol",
    "find_referencing_symbols",
    "find_implementations",
    "find_declaration",
    "get_diagnostics_for_file",
    "initial_instructions",
];

const SERENA_SOURCE: &str = "git+https://github.com/oraios/serena";

/// A generated code-lookup setup: the files written under `home` and the MCP
/// server entry a worker's harness is given.
#[derive(Clone, Debug)]
pub struct CodeLookup {
    pub home: PathBuf,
    pub serena_config: PathBuf,
    pub project_yml: PathBuf,
    pub mcp: Value,
}

fn q(s: &str) -> String {
    // A JSON string is a valid YAML double-quoted scalar.
    Value::String(s.to_string()).to_string()
}

fn list(items: &[&str]) -> String {
    items.iter().map(|i| format!("- {}\n", q(i))).collect()
}

/// Write the read-only code-lookup config for `project` under `home` and return
/// the MCP entry. The Serena data folder (memories, cache, project.yml) lives
/// under `home`, never in the project: `home` inside `project` is refused.
pub fn code_lookup_config(
    project: &Path,
    home: &Path,
    languages: &[&str],
) -> io::Result<CodeLookup> {
    if languages.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no language given"));
    }
    if home.starts_with(project) || project.starts_with(home) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the Serena home must lie outside the project",
        ));
    }
    let folder = project
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "project has no folder name"))?;
    let home_s = home.to_string_lossy().into_owned();
    let project_s = project.to_string_lossy().into_owned();

    let data = home.join("data").join(folder);
    fs::create_dir_all(&data)?;

    let serena_config = home.join("serena_config.yml");
    fs::write(
        &serena_config,
        format!(
            "projects: []\nproject_serena_folder_location: {}\nweb_dashboard: false\nweb_dashboard_open_on_launch: false\ngui_log_window: false\n",
            q(&format!("{home_s}/data/$projectFolderName"))
        ),
    )?;

    let project_yml = data.join("project.yml");
    fs::write(
        &project_yml,
        format!(
            "project_name: {}\nread_only: true\nlanguage_servers:\n{}excluded_tools:\n{}",
            q(folder),
            list(languages),
            list(&SERENA_MEMORY_TOOLS)
        ),
    )?;

    let mcp = json!({
        "mcpServers": {
            "code-lookup": {
                "command": "uvx",
                "args": [
                    "--from", SERENA_SOURCE, "serena", "start-mcp-server",
                    "--project", project_s,
                    "--context", "ide",
                    "--agent-interface", "tools",
                    "--enable-web-dashboard", "false",
                    "--open-web-dashboard", "false"
                ],
                "env": { "SERENA_HOME": home_s }
            }
        }
    });
    Ok(CodeLookup { home: home.to_path_buf(), serena_config, project_yml, mcp })
}

/// The tool names a generated project.yml disables (its `excluded_tools` list).
pub fn excluded_tools(project_yml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut on = false;
    for line in project_yml.lines() {
        if let Some(item) = line.strip_prefix("- ") {
            if on {
                out.push(item.trim().trim_matches('"').to_string());
            }
        } else {
            on = line.trim_end() == "excluded_tools:";
        }
    }
    out
}

/// Tools the generated config leaves enabled: the lookup set minus anything
/// excluded, and none at all of the editing set when `read_only` holds.
pub fn enabled_tools(project_yml: &str) -> Vec<String> {
    let excluded = excluded_tools(project_yml);
    let read_only = project_yml.lines().any(|l| l.trim() == "read_only: true");
    CODE_LOOKUP_TOOLS
        .iter()
        .filter(|t| !excluded.iter().any(|e| e == *t))
        .filter(|t| !(read_only && SERENA_EDIT_TOOLS.contains(t)))
        .map(|t| t.to_string())
        .collect()
}
