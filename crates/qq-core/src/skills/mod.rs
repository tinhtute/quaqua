//! Role skill sets from one source (R12). `SKILLS` is the only place that says
//! which role receives which skill or playbook file of the workspace template;
//! a file with no row reaches no role, and the Supervisor agent has no row, so
//! it gets nothing. `inject` hands one role's set to one driver's CLI: claude
//! gets the files in its skills directory, a native driver without a skills
//! directory (pi, agy) gets one rules file, and the generic driver gets the
//! set as prompt text.

use crate::authority::Role;
use crate::cli::{Install, Registry};
use crate::daemon::Daemon;
use crate::driver::{exclude_from_git, write_atomic, HarnessDriver};
use std::io;
use std::path::{Path, PathBuf};

pub const INSTALL: Install = install;

fn install(_: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    Ok(())
}

/// One skill or playbook file of `templates/workspace/`.
pub struct Skill {
    /// Relative to `templates/workspace/`.
    pub path: &'static str,
    pub roles: &'static [Role],
    pub text: &'static str,
}

const COS_LEAD: &[Role] = &[Role::Cos, Role::Lead];
const PEER: &[Role] = &[Role::Peer];

/// The role table. Orchestration files go to the CoS and the Lead, implementation
/// playbooks to the Peer (CoS ruling on T56). Principles are reference files and
/// have no row. `feature` stays whole in the Peer set until T59 splits its
/// Orchestrator section.
pub const SKILLS: &[Skill] = &[
    Skill {
        path: ".claude/skills/herdr-orchestration/SKILL.md",
        roles: COS_LEAD,
        text: include_str!("../../templates/workspace/.claude/skills/herdr-orchestration/SKILL.md"),
    },
    Skill {
        path: ".claude/skills/herdr-orchestration/references/operations.md",
        roles: COS_LEAD,
        text: include_str!(
            "../../templates/workspace/.claude/skills/herdr-orchestration/references/operations.md"
        ),
    },
    Skill {
        path: ".claude/skills/herdr-orchestration/references/recovery.md",
        roles: COS_LEAD,
        text: include_str!(
            "../../templates/workspace/.claude/skills/herdr-orchestration/references/recovery.md"
        ),
    },
    Skill {
        path: ".claude/skills/herdr-orchestration/references/judgment.md",
        roles: COS_LEAD,
        text: include_str!(
            "../../templates/workspace/.claude/skills/herdr-orchestration/references/judgment.md"
        ),
    },
    Skill {
        path: "playbooks/multi-phase-plan.md",
        roles: COS_LEAD,
        text: include_str!("../../templates/workspace/playbooks/multi-phase-plan.md"),
    },
    Skill {
        path: "playbooks/shipping.md",
        roles: COS_LEAD,
        text: include_str!("../../templates/workspace/playbooks/shipping.md"),
    },
    Skill {
        path: "playbooks/babysit.md",
        roles: COS_LEAD,
        text: include_str!("../../templates/workspace/playbooks/babysit.md"),
    },
    Skill {
        path: "playbooks/session-pickup.md",
        roles: COS_LEAD,
        text: include_str!("../../templates/workspace/playbooks/session-pickup.md"),
    },
    Skill {
        path: "playbooks/pause-safely.md",
        roles: COS_LEAD,
        text: include_str!("../../templates/workspace/playbooks/pause-safely.md"),
    },
    Skill {
        path: "playbooks/worktree-cleanup.md",
        roles: COS_LEAD,
        text: include_str!("../../templates/workspace/playbooks/worktree-cleanup.md"),
    },
    Skill {
        path: "playbooks/eval.md",
        roles: COS_LEAD,
        text: include_str!("../../templates/workspace/playbooks/eval.md"),
    },
    Skill {
        path: "playbooks/trace-forensics.md",
        roles: COS_LEAD,
        text: include_str!("../../templates/workspace/playbooks/trace-forensics.md"),
    },
    Skill {
        path: "playbooks/feature.md",
        roles: PEER,
        text: include_str!("../../templates/workspace/playbooks/feature.md"),
    },
    Skill {
        path: "playbooks/bug-fix.md",
        roles: PEER,
        text: include_str!("../../templates/workspace/playbooks/bug-fix.md"),
    },
    Skill {
        path: "playbooks/refactoring.md",
        roles: PEER,
        text: include_str!("../../templates/workspace/playbooks/refactoring.md"),
    },
    Skill {
        path: "playbooks/perf-issue.md",
        roles: PEER,
        text: include_str!("../../templates/workspace/playbooks/perf-issue.md"),
    },
    Skill {
        path: "playbooks/investigation.md",
        roles: PEER,
        text: include_str!("../../templates/workspace/playbooks/investigation.md"),
    },
    Skill {
        path: "playbooks/prototype.md",
        roles: PEER,
        text: include_str!("../../templates/workspace/playbooks/prototype.md"),
    },
    Skill {
        path: "playbooks/hillclimb.md",
        roles: PEER,
        text: include_str!("../../templates/workspace/playbooks/hillclimb.md"),
    },
    Skill {
        path: "playbooks/visual-parity.md",
        roles: PEER,
        text: include_str!("../../templates/workspace/playbooks/visual-parity.md"),
    },
    Skill {
        path: "playbooks/runtime-forensics.md",
        roles: PEER,
        text: include_str!("../../templates/workspace/playbooks/runtime-forensics.md"),
    },
    Skill {
        path: "playbooks/figure-it-out.md",
        roles: PEER,
        text: include_str!("../../templates/workspace/playbooks/figure-it-out.md"),
    },
];

/// The files of one role's set, in table order. Empty for the Supervisor.
pub fn set_for(role: Role) -> Vec<&'static Skill> {
    SKILLS.iter().filter(|s| s.roles.contains(&role)).collect()
}

/// How a set reached the agent's CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The role has no skills: nothing was written or sent.
    Nothing,
    /// Skill files written under the worktree (paths relative to it).
    Files(Vec<PathBuf>),
    /// One rules file written under the worktree (path relative to it).
    RulesFile(PathBuf),
    /// Text to put at the head of the agent's prompt.
    Prompt(String),
}

/// Relative to the worktree; untracked.
const RULES_REL: &str = ".qq/role-skills.md";

enum Channel {
    Files,
    Rules,
    Prompt,
}

/// The channel is chosen by what the driver is, not by harness name: the
/// generic driver has no hook and no files of its own, claude is the one native
/// harness with a skills directory, every other native reads a rules file.
fn channel(driver: &dyn HarnessDriver) -> Channel {
    if driver.driver() == crate::config::Driver::Generic {
        Channel::Prompt
    } else if driver.herdr_kind() == "claude" {
        Channel::Files
    } else {
        Channel::Rules
    }
}

/// Where a file lands in claude's skills directory. Skills keep their path;
/// a playbook becomes a skill named `playbook-<name>` whose description is its
/// `when` line.
fn claude_target(s: &Skill) -> (PathBuf, String) {
    let Some(name) = s
        .path
        .strip_prefix("playbooks/")
        .and_then(|n| n.strip_suffix(".md"))
    else {
        return (PathBuf::from(s.path), s.text.to_string());
    };
    let mut when = "";
    let mut body = s.text;
    if let Some(rest) = s.text.strip_prefix("---\n") {
        if let Some((head, tail)) = rest.split_once("\n---\n") {
            when = head
                .lines()
                .find_map(|l| l.strip_prefix("when:"))
                .map_or("", str::trim);
            body = tail;
        }
    }
    let text = format!(
        "---\nname: playbook-{name}\ndescription: {when}\n---\n{}",
        body.trim_start_matches('\n')
    );
    (
        PathBuf::from(format!(".claude/skills/playbook-{name}/SKILL.md")),
        text,
    )
}

fn joined(set: &[&Skill]) -> String {
    let mut out = String::new();
    for s in set {
        out.push_str(&format!(
            "<!-- skill: {} -->\n{}\n",
            s.path,
            s.text.trim_end()
        ));
    }
    out
}

/// Delivers `role`'s set to `driver`'s CLI. Files and the rules file are
/// written under `worktree` and kept out of git; a Prompt is returned for the
/// caller to prepend.
pub fn inject(driver: &dyn HarnessDriver, role: Role, worktree: &Path) -> io::Result<Delivery> {
    let set = set_for(role);
    if set.is_empty() {
        return Ok(Delivery::Nothing);
    }
    match channel(driver) {
        Channel::Prompt => Ok(Delivery::Prompt(joined(&set))),
        Channel::Rules => {
            write_atomic(&worktree.join(RULES_REL), joined(&set).as_bytes())?;
            exclude_from_git(worktree, "/.qq/role-skills.md")?;
            Ok(Delivery::RulesFile(PathBuf::from(RULES_REL)))
        }
        Channel::Files => {
            let mut written = Vec::new();
            for s in set {
                let (rel, text) = claude_target(s);
                write_atomic(&worktree.join(&rel), text.as_bytes())?;
                let dir = rel.parent().unwrap_or(Path::new(""));
                exclude_from_git(worktree, &format!("/{}/", dir.display()))?;
                written.push(rel);
            }
            Ok(Delivery::Files(written))
        }
    }
}
