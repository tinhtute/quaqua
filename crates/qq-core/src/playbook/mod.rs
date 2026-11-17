//! `qq playbook list|show`: parse and validate the workspace playbooks (ported
//! from the old crate; the workspace root is the parent of `runs/`).

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{CliError, Command, Env, Install, Registry};
use crate::daemon::Daemon;

/// A single step in the `## Worker` section of a playbook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Step identifier (the number from the numbered list).
    pub id: u32,
    /// Full text of the step.
    pub text: String,
    /// Principle names cited in this step (e.g. `["principle-fixture"]`).
    pub cited_principles: Vec<String>,
}

/// A parsed playbook file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playbook {
    /// Playbook name from frontmatter `name:`.
    pub name: String,
    /// Routing trigger from frontmatter `when:`.
    pub when: String,
    /// Orchestrator section content (including the `## Orchestrator` heading).
    pub orchestrator: String,
    /// Worker steps parsed from the numbered list.
    pub steps: Vec<Step>,
    /// All distinct cited principle leaves across worker steps (e.g. `["principle-fixture"]`).
    pub cited_principles: Vec<String>,
    /// Raw worker section text, if any.
    pub worker_text: String,
    /// Path to the source file, if loaded from disk.
    pub source_path: Option<PathBuf>,
}

impl Playbook {
    /// Return the step IDs as a vector of integers.
    pub fn step_ids(&self) -> Vec<u32> {
        self.steps.iter().map(|s| s.id).collect()
    }

    /// Return the body of the Orchestrator section without the `## Orchestrator` header.
    pub fn orchestrator_body(&self) -> &str {
        let trimmed = self.orchestrator.trim();
        if let Some(stripped) = trimmed.strip_prefix("## Orchestrator") {
            stripped.trim()
        } else {
            trimmed
        }
    }
}

/// Errors occurring during playbook parsing and resolution.
#[derive(Debug)]
pub enum PlaybookError {
    Io(std::io::Error),
    MissingFrontmatter,
    MissingName,
    MissingWhen,
    MissingOrchestratorSection,
    MissingWorkerSection,
    UnknownPrinciple {
        principle: String,
        path: PathBuf,
    },
    LeafCapExceeded {
        count: usize,
        max: usize,
        leaves: Vec<String>,
    },
    NotFound(String),
    ParseError(String),
    DirectoryNotFound {
        kind: &'static str,
        path: PathBuf,
    },
    WorkspaceError(String),
}

impl fmt::Display for PlaybookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::MissingFrontmatter => write!(f, "missing frontmatter delimiter '---'"),
            Self::MissingName => write!(f, "missing 'name:' in playbook frontmatter"),
            Self::MissingWhen => write!(f, "missing 'when:' in playbook frontmatter"),
            Self::MissingOrchestratorSection => write!(f, "missing '## Orchestrator' section"),
            Self::MissingWorkerSection => write!(f, "missing '## Worker' section"),
            Self::UnknownPrinciple { principle, path } => {
                write!(
                    f,
                    "unknown principle '{principle}': file '{}' not found",
                    path.display()
                )
            }
            Self::LeafCapExceeded { count, max, leaves } => {
                write!(
                    f,
                    "Worker section cites {count} leaves besides principle-prove-it-works (max {max}): {}",
                    leaves.join(", ")
                )
            }
            Self::NotFound(name) => write!(f, "playbook '{name}' not found"),
            Self::ParseError(msg) => write!(f, "playbook parse error: {msg}"),
            Self::DirectoryNotFound { kind, path } => {
                write!(f, "{kind} directory not found: '{}'", path.display())
            }
            Self::WorkspaceError(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for PlaybookError {}

impl From<std::io::Error> for PlaybookError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Unquote a string scalar if wrapped in single or double quotes.
fn unquote(s: &str) -> &str {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

/// Parse a numbered list item line under `## Worker`.
/// Matches: `^\s*(?:-\s*)?(\d+)[.):]\s*(.*)$`.
fn parse_step_line(line: &str) -> Option<(u32, &str)> {
    let trimmed = line.trim();
    let s = trimmed.strip_prefix("- ").unwrap_or(trimmed);
    let digits_end = s.find(|c: char| !c.is_ascii_digit())?;
    if digits_end == 0 {
        return None;
    }
    let id: u32 = s[..digits_end].parse().ok()?;
    let rest = &s[digits_end..];
    if let Some(after) = rest
        .strip_prefix('.')
        .or_else(|| rest.strip_prefix(')'))
        .or_else(|| rest.strip_prefix(':'))
    {
        if after.is_empty() || after.starts_with(char::is_whitespace) {
            return Some((id, after.trim()));
        }
    }
    None
}

/// Extract all `principle-<name>` citations from text.
pub fn extract_principle_citations(text: &str) -> Vec<String> {
    let mut citations = Vec::new();
    let marker = "principle-";
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if let Some(pos) = text[i..].find(marker) {
            let start = i + pos;
            // Check preceding char is not alphanumeric or hyphen
            if start > 0 {
                let prev = text[..start].chars().next_back().unwrap();
                if prev.is_ascii_alphanumeric() || prev == '-' {
                    i = start + marker.len();
                    continue;
                }
            }
            let name_start = start + marker.len();
            let mut end = name_start;
            while end < text.len() {
                let c = text[end..].chars().next().unwrap();
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    end += c.len_utf8();
                } else {
                    break;
                }
            }
            let mut raw_name = &text[name_start..end];
            // Strip trailing punctuation / hyphens
            raw_name = raw_name.trim_end_matches(|c: char| {
                matches!(
                    c,
                    '.' | ',' | ')' | ']' | '}' | ':' | ';' | '\'' | '"' | '`' | '-' | '_'
                )
            });

            if !raw_name.is_empty() {
                let citation = format!("principle-{raw_name}");
                if !citations.contains(&citation) {
                    citations.push(citation);
                }
            }
            i = end;
        } else {
            break;
        }
    }
    citations
}

/// Parse a playbook string and validate its principles against `principles_dir`.
pub fn parse_playbook(content: &str, principles_dir: &Path) -> Result<Playbook, PlaybookError> {
    let mut lines = content.lines().peekable();

    // 1. Parse frontmatter
    // Skip initial empty lines
    while let Some(line) = lines.peek() {
        if line.trim().is_empty() {
            lines.next();
        } else {
            break;
        }
    }

    let first = lines.next().ok_or(PlaybookError::MissingFrontmatter)?;
    if first.trim() != "---" {
        return Err(PlaybookError::MissingFrontmatter);
    }

    let mut name = String::new();
    let mut when = String::new();
    let mut closed_frontmatter = false;

    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed == "---" {
            closed_frontmatter = true;
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("name:") {
            name = unquote(rest).to_string();
        } else if let Some(rest) = trimmed.strip_prefix("when:") {
            when = unquote(rest).to_string();
        }
    }

    if !closed_frontmatter {
        return Err(PlaybookError::MissingFrontmatter);
    }
    if name.is_empty() {
        return Err(PlaybookError::MissingName);
    }
    if when.is_empty() {
        return Err(PlaybookError::MissingWhen);
    }

    // 2. Parse sections: ## Orchestrator and ## Worker
    enum Section {
        None,
        Orchestrator,
        Worker,
        Other,
    }

    let mut current_section = Section::None;
    let mut orchestrator_lines: Vec<String> = Vec::new();
    let mut worker_lines: Vec<String> = Vec::new();
    let mut seen_orchestrator = false;
    let mut seen_worker = false;
    let mut in_fence = false;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        if !in_fence && trimmed.starts_with("## ") {
            let heading = trimmed[3..].trim();
            if heading.eq_ignore_ascii_case("orchestrator") {
                current_section = Section::Orchestrator;
                seen_orchestrator = true;
                continue;
            } else if heading.eq_ignore_ascii_case("worker") {
                current_section = Section::Worker;
                seen_worker = true;
                continue;
            } else {
                current_section = Section::Other;
                continue;
            }
        }

        match current_section {
            Section::Orchestrator => orchestrator_lines.push(line.to_string()),
            Section::Worker => worker_lines.push(line.to_string()),
            _ => {}
        }
    }

    if !seen_orchestrator {
        return Err(PlaybookError::MissingOrchestratorSection);
    }
    if !seen_worker {
        return Err(PlaybookError::MissingWorkerSection);
    }

    let orchestrator_body = orchestrator_lines.join("\n").trim().to_string();
    let orchestrator = if orchestrator_body.is_empty() {
        "## Orchestrator".to_string()
    } else {
        format!("## Orchestrator\n\n{orchestrator_body}")
    };

    // Validate principle citations in ## Orchestrator against principles_dir
    let orch_citations = extract_principle_citations(&orchestrator_body);
    for citation in &orch_citations {
        let leaf_name = citation
            .strip_prefix("principle-")
            .unwrap_or(citation.as_str());
        let leaf_path = principles_dir.join(format!("{leaf_name}.md"));
        if !leaf_path.is_file() {
            return Err(PlaybookError::UnknownPrinciple {
                principle: citation.clone(),
                path: leaf_path,
            });
        }
    }

    let worker_text = worker_lines.join("\n").trim().to_string();

    // 3. Parse worker steps
    let mut steps: Vec<Step> = Vec::new();
    let mut current_id: Option<u32> = None;
    let mut current_step_lines: Vec<String> = Vec::new();

    for line in &worker_lines {
        if let Some((id, step_text)) = parse_step_line(line) {
            if let Some(prev_id) = current_id {
                let full_text = current_step_lines.join("\n").trim().to_string();
                let cited = extract_principle_citations(&full_text);
                steps.push(Step {
                    id: prev_id,
                    text: full_text,
                    cited_principles: cited,
                });
                current_step_lines.clear();
            }
            current_id = Some(id);
            current_step_lines.push(step_text.to_string());
        } else if current_id.is_some() {
            current_step_lines.push(line.clone());
        }
    }
    if let Some(prev_id) = current_id {
        let full_text = current_step_lines.join("\n").trim().to_string();
        let cited = extract_principle_citations(&full_text);
        steps.push(Step {
            id: prev_id,
            text: full_text,
            cited_principles: cited,
        });
    }

    // 4. Validate principles resolution against principles_dir
    // 4. Validate principles resolution against principles_dir across entire Worker section
    let worker_citations = extract_principle_citations(&worker_text);
    let mut all_cited_principles: Vec<String> = Vec::new();
    for citation in &worker_citations {
        if !all_cited_principles.contains(citation) {
            all_cited_principles.push(citation.clone());
        }
        let leaf_name = citation
            .strip_prefix("principle-")
            .unwrap_or(citation.as_str());
        let leaf_path = principles_dir.join(format!("{leaf_name}.md"));
        if !leaf_path.is_file() {
            return Err(PlaybookError::UnknownPrinciple {
                principle: citation.clone(),
                path: leaf_path,
            });
        }
    }

    // 5. Validate the ≤3 leaf cap (excluding `principle-prove-it-works`)
    let mut unique_leaves = BTreeSet::new();
    for citation in &all_cited_principles {
        let leaf_name = citation
            .strip_prefix("principle-")
            .unwrap_or(citation.as_str());
        unique_leaves.insert(leaf_name.to_string());
    }
    let non_exempt_leaves: Vec<String> = unique_leaves
        .iter()
        .filter(|name| *name != "prove-it-works")
        .map(|s| format!("principle-{s}"))
        .collect();

    if non_exempt_leaves.len() > 3 {
        return Err(PlaybookError::LeafCapExceeded {
            count: non_exempt_leaves.len(),
            max: 3,
            leaves: non_exempt_leaves,
        });
    }
    // worker_text is already extracted above

    Ok(Playbook {
        name,
        when,
        orchestrator,
        steps,
        cited_principles: all_cited_principles,
        worker_text,
        source_path: None,
    })
}

/// Load and parse a playbook from a file path.
pub fn load_playbook(path: &Path, principles_dir: &Path) -> Result<Playbook, PlaybookError> {
    let content = fs::read_to_string(path)?;
    let mut pb = parse_playbook(&content, principles_dir)?;
    pb.source_path = Some(path.to_path_buf());
    Ok(pb)
}

/// Load and validate all playbooks in a directory.
pub fn load_playbooks(
    playbooks_dir: &Path,
    principles_dir: &Path,
) -> Result<Vec<Playbook>, PlaybookError> {
    if !playbooks_dir.is_dir() {
        return Err(PlaybookError::DirectoryNotFound {
            kind: "playbooks",
            path: playbooks_dir.to_path_buf(),
        });
    }
    if !principles_dir.is_dir() {
        return Err(PlaybookError::DirectoryNotFound {
            kind: "principles",
            path: principles_dir.to_path_buf(),
        });
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(playbooks_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().and_then(|ext| ext.to_str()) == Some("md")
                && !p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .starts_with('.')
        })
        .collect();
    entries.sort();

    let mut playbooks = Vec::new();
    for path in entries {
        let pb = load_playbook(&path, principles_dir)?;
        playbooks.push(pb);
    }
    playbooks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(playbooks)
}

/// Find a playbook by name in the playbooks directory.
pub fn find_playbook(
    name: &str,
    playbooks_dir: &Path,
    principles_dir: &Path,
) -> Result<Playbook, PlaybookError> {
    if !playbooks_dir.is_dir() {
        return Err(PlaybookError::DirectoryNotFound {
            kind: "playbooks",
            path: playbooks_dir.to_path_buf(),
        });
    }
    if !principles_dir.is_dir() {
        return Err(PlaybookError::DirectoryNotFound {
            kind: "principles",
            path: principles_dir.to_path_buf(),
        });
    }
    let direct_path = playbooks_dir.join(format!("{name}.md"));
    if direct_path.is_file() {
        let pb = load_playbook(&direct_path, principles_dir)?;
        if pb.name == name {
            return Ok(pb);
        }
    }
    let all = load_playbooks(playbooks_dir, principles_dir)?;
    for pb in all {
        if pb.name == name {
            return Ok(pb);
        }
    }
    Err(PlaybookError::NotFound(name.to_string()))
}

/// Resolve the expected path of a principle leaf file given its name.
pub fn resolve_principle_path(name: &str, principles_dir: &Path) -> PathBuf {
    let leaf_name = name.strip_prefix("principle-").unwrap_or(name);
    principles_dir.join(format!("{leaf_name}.md"))
}

/// Read the content of a principle leaf file given its name.
pub fn read_principle(name: &str, principles_dir: &Path) -> Result<String, PlaybookError> {
    let path = resolve_principle_path(name, principles_dir);
    if !path.is_file() {
        return Err(PlaybookError::UnknownPrinciple {
            principle: name.to_string(),
            path,
        });
    }
    Ok(fs::read_to_string(&path)?)
}

/// The directory `name` under the workspace (the parent of `runs/`), unless the
/// env var `var` or an explicit path names another.
fn resolve_dir(
    explicit: Option<&Path>,
    var: &str,
    env: &Env,
    name: &str,
) -> Result<PathBuf, PlaybookError> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Some(val) = std::env::var_os(var).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(val));
    }
    let root = env.runs_dir.parent().ok_or_else(|| {
        PlaybookError::WorkspaceError(format!(
            "runs dir {} has no workspace parent",
            env.runs_dir.display()
        ))
    })?;
    Ok(root.join(name))
}

/// `<name> — <when>` per playbook.
pub fn run_list(playbooks_dir: &Path, principles_dir: &Path) -> Result<String, PlaybookError> {
    let mut out = String::new();
    for pb in load_playbooks(playbooks_dir, principles_dir)? {
        out.push_str(&format!("{} — {}\n", pb.name, pb.when));
    }
    Ok(out)
}

/// Only the `## Orchestrator` section of `name`.
pub fn run_show(
    name: &str,
    playbooks_dir: &Path,
    principles_dir: &Path,
) -> Result<String, PlaybookError> {
    let pb = find_playbook(name, playbooks_dir, principles_dir)?;
    Ok(format!("{}\n", pb.orchestrator.trim()))
}

/// The entry `cli::MODULES` lists.
pub const INSTALL: Install = install;

fn install(registry: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "playbook",
        help: "playbook list | show <name> [--dir D] [--principles-dir D]",
        run: playbook_cmd,
    })
}

pub fn playbook_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    let usage = || {
        CliError::new(
            2,
            "usage: qq playbook list | show <name> [--dir D] [--principles-dir D]",
        )
    };
    let (Some(sub), rest) = (args.first(), args.get(1..).unwrap_or_default()) else {
        return Err(usage());
    };
    let mut name = None;
    let (mut dir, mut pdir) = (None, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => dir = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--principles-dir" => pdir = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            s if !s.starts_with("--") && name.is_none() => name = Some(s.to_string()),
            _ => return Err(usage()),
        }
    }
    let fail = |e: PlaybookError| CliError::new(1, e.to_string());
    let pb = resolve_dir(dir.as_deref(), "QUAQUA_PLAYBOOKS_DIR", env, "playbooks").map_err(fail)?;
    let pr =
        resolve_dir(pdir.as_deref(), "QUAQUA_PRINCIPLES_DIR", env, "principles").map_err(fail)?;
    let out = match (sub.as_str(), name) {
        ("list", None) => run_list(&pb, &pr),
        ("show", Some(n)) => run_show(&n, &pb, &pr),
        _ => return Err(usage()),
    };
    out.map(|s| s.trim_end().to_string()).map_err(fail)
}
