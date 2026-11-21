//! Project protocol reader (plan task T55).
//!
//! A project keeps its tactics in the fenced `toml` block of
//! `WORKSPACE_PROTOCOL.md`. The file is read from the local default
//! branch only (never the worktree, never a fetch); no file means the engine
//! default. A key can only tighten or describe: the known keys are
//! `worker_cap` (clamped to [`CEILING`]) and `notes`; any other key is
//! an error, so no protocol key can loosen an engine rule. The text goes to
//! the Lead prompt ([`lead_prompt_section`]); a Peer gets nothing.

use std::path::Path;
use std::process::{Command, Stdio};

use rusqlite::Connection;

use super::{refuse, AuthError, Decision, Role};

/// File name read from the default branch root.
pub const FILE: &str = "WORKSPACE_PROTOCOL.md";
/// Worker cap when the protocol is absent or silent.
pub const DEFAULT_CAP: u32 = 4;
/// No protocol raises the cap above this.
pub const CEILING: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protocol {
    /// Active Peers a project may hold at once, in 1..=[`CEILING`].
    pub worker_cap: u32,
    /// Free text for the Lead.
    pub notes: Option<String>,
    /// The block as written; empty for the engine default.
    pub source: String,
}

impl Default for Protocol {
    fn default() -> Self {
        Protocol {
            worker_cap: DEFAULT_CAP,
            notes: None,
            source: String::new(),
        }
    }
}

/// The first fenced `toml` block of `md`, if any.
fn toml_block(md: &str) -> Option<String> {
    let mut lines = md.lines();
    lines.find(|l| l.trim_end() == "```toml")?;
    let body: Vec<&str> = lines.take_while(|l| l.trim_end() != "```").collect();
    Some(body.join("\n"))
}

/// Parses the protocol document. No block: the engine default.
pub fn parse(md: &str) -> Result<Protocol, AuthError> {
    let Some(block) = toml_block(md) else {
        return Ok(Protocol::default());
    };
    let table: toml::Table = block
        .parse()
        .map_err(|e| AuthError(format!("{FILE}: {e}")))?;
    let mut p = Protocol {
        source: block,
        ..Protocol::default()
    };
    for (k, v) in &table {
        match k.as_str() {
            "worker_cap" => match v.as_integer() {
                Some(n) if n >= 1 => p.worker_cap = u32::try_from(n).unwrap_or(CEILING).min(CEILING),
                _ => return Err(AuthError(format!("{FILE}: worker_cap must be an integer >= 1"))),
            },
            "notes" => match v.as_str() {
                Some(s) => p.notes = Some(s.to_string()),
                None => return Err(AuthError(format!("{FILE}: notes must be a string"))),
            },
            other => {
                return Err(AuthError(format!(
                    "{FILE}: key '{other}' is not a protocol key (a protocol cannot change an engine rule)"
                )))
            }
        }
    }
    Ok(p)
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// The local default branch: origin's HEAD symref if known, else
/// `init.defaultBranch`, else a local `main` or `master`.
fn default_branch(repo: &Path) -> Option<String> {
    let has = |b: &str| {
        git(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
        )
        .is_some()
    };
    git(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .and_then(|h| h.strip_prefix("origin/").map(str::to_string))
    .into_iter()
    .chain(git(repo, &["config", "init.defaultBranch"]))
    .chain(["main".to_string(), "master".to_string()])
    .find(|b| !b.is_empty() && has(b))
}

/// Reads the protocol of the repo at `repo` from its local default branch.
/// No default branch or no file: the engine default; a broken block: an error.
pub fn read(repo: &Path) -> Result<Protocol, AuthError> {
    let Some(branch) = default_branch(repo) else {
        return Ok(Protocol::default());
    };
    let spec = format!("refs/heads/{branch}:{FILE}");
    if git(repo, &["cat-file", "-e", &spec]).is_none() {
        // cat-file -e prints nothing; a missing blob and a failing git both end here.
        let listed = git(
            repo,
            &[
                "ls-tree",
                "--name-only",
                &format!("refs/heads/{branch}"),
                "--",
                FILE,
            ],
        );
        if listed.as_deref().is_none_or(str::is_empty) {
            return Ok(Protocol::default());
        }
    }
    let md = git(repo, &["show", &spec])
        .ok_or_else(|| AuthError(format!("{FILE}: cannot read {spec}")))?;
    parse(&md)
}

/// The protocol text for the prompt of a `role`: a Lead gets the rendered
/// protocol, every other role gets nothing.
pub fn lead_prompt_section(role: Role, p: &Protocol) -> Option<String> {
    (role == Role::Lead).then(|| render(p))
}

fn render(p: &Protocol) -> String {
    let mut s = format!(
        "Project protocol ({FILE}): worker cap {} (engine ceiling {CEILING}).",
        p.worker_cap
    );
    if let Some(n) = &p.notes {
        s.push('\n');
        s.push_str(n);
    }
    s
}

/// May `caller` spawn one more Peer into `project`? Refused (and journaled) when
/// the project already holds `worker_cap` active Peers.
pub fn check_worker_cap(
    conn: &Connection,
    caller: &str,
    project: i64,
    p: &Protocol,
) -> Result<Decision, AuthError> {
    let active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent WHERE project_id = ?1 AND role = 'peer' AND ended_ms IS NULL",
        [project],
        |r| r.get(0),
    )?;
    if active >= i64::from(p.worker_cap) {
        return refuse(
            conn,
            caller,
            "spawn",
            "peer",
            &format!("worker cap {} reached ({active} active)", p.worker_cap),
        );
    }
    Ok(Decision::Allowed)
}
