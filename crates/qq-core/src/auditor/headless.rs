//! The one-shot, read-only command line of a harness.
//!
//! This is per-harness behaviour and belongs in `driver/` (a `HarnessDriver`
//! method); it lives here only because `driver/` is outside this module's
//! remit. The harness comes from the driver's `herdr_kind()`, model and effort
//! from config via the driver's `launch_args`. Move this match into `driver/`
//! when the Supervisor (T63) absorbs scheduled audits.

/// Read-only tools the audit may use: file reads plus herdr, git and sqlite
/// inspection commands. Nothing here mutates workspace, repo, herdr or database state.
const READ_ONLY_TOOLS: &str = concat!(
    "Read,Grep,Glob,",
    "Bash(git log:*),Bash(git diff:*),Bash(git show:*),Bash(git status:*),",
    "Bash(git rev-parse:*),Bash(git rev-list:*),Bash(git branch --list:*),",
    "Bash(git worktree list:*),Bash(git ls-files:*),Bash(git cat-file:*),",
    "Bash(herdr status:*),Bash(herdr agent list:*),Bash(herdr agent get:*),",
    "Bash(herdr agent read:*),Bash(herdr pane list:*),Bash(herdr pane get:*),",
    "Bash(herdr pane read:*),Bash(herdr tab list:*),Bash(herdr workspace list:*),",
    "Bash(sqlite3 -readonly:*)"
);

/// A program and its argv, prompt included.
pub struct Headless {
    pub program: String,
    pub args: Vec<String>,
}

/// `None`: no headless form is known for this agent kind.
pub fn command(herdr_kind: &str, launch_args: Vec<String>, prompt: &str) -> Option<Headless> {
    match herdr_kind {
        "claude" => {
            let mut args = vec!["-p".to_string()];
            args.extend(launch_args);
            args.extend(
                [
                    "--permission-mode",
                    "dontAsk",
                    "--setting-sources",
                    "",
                    "--allowedTools",
                    READ_ONLY_TOOLS,
                    "--disallowedTools",
                    "Bash(git *--output*)",
                    "--",
                ]
                .map(String::from),
            );
            args.push(prompt.to_string());
            Some(Headless {
                program: herdr_kind.to_string(),
                args,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> String {
        let h = command("claude", vec![], "p").unwrap();
        let i = h.args.iter().position(|a| a == "--allowedTools").unwrap();
        h.args[i + 1].clone()
    }

    #[test]
    fn auditor_allowlist_has_herdr_git_sqlite_reads() {
        let a = allowed();
        for want in [
            "Bash(herdr agent list:*)",
            "Bash(herdr pane read:*)",
            "Bash(herdr status:*)",
            "Bash(git status:*)",
            "Bash(git rev-parse:*)",
            "Bash(git worktree list:*)",
            "Bash(sqlite3 -readonly:*)",
        ] {
            assert!(a.split(',').any(|t| t == want), "missing {want} in {a}");
        }
    }

    #[test]
    fn auditor_allowlist_has_no_mutating_commands() {
        let a = allowed();
        for bad in ["herdr pane run", "herdr agent send", "git push", "git commit", "git checkout", "Bash(sqlite3:*)"] {
            assert!(!a.contains(bad), "{bad} must not be allowed: {a}");
        }
    }
}
