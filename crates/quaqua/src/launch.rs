//! Agent launch engine (`Launch`, `resolve_launch`, `build_launch_cmd`, `claude_trust_dialog_up`).
//!
//! Shared across task workers (`worker::spawn`) and project leads (`lead::spawn`).

use crate::config::Kind;
use crate::herdr::HerdrOps;
use std::path::Path;

/// The harness/model/effort a spawn launches with, resolved from team.toml.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub harness: String,
    pub model: String,
    pub effort: String,
}

/// Resolve a kind's launch from team.toml, failing closed when the kind has no
/// model or effort, or the harness cannot take that effort value. No defaults:
/// team.toml is the only source (the old `unwrap_or("medium"/"high")` are gone).
pub fn resolve_launch(
    kind: &Kind,
    kind_name: &str,
    cfg: &crate::config::Config,
) -> Result<Launch, (i32, String)> {
    let Some(harness) = crate::harness::Harness::parse(&kind.harness) else {
        return Err((
            1,
            format!(
                "error: unsupported harness '{}' for kind '{kind_name}'\n",
                kind.harness
            ),
        ));
    };
    if kind.model.trim().is_empty() {
        return Err((
            1,
            format!("error: kind '{kind_name}' has no model in team.toml\n"),
        ));
    }
    let effort = match kind.effort.as_deref() {
        Some(e) if !e.trim().is_empty() => e,
        _ => {
            return Err((
                1,
                format!("error: kind '{kind_name}' has no effort in team.toml\n"),
            ))
        }
    };
    if !harness.effort_allowed(effort) {
        return Err((
            1,
            format!(
                "error: harness '{}' does not take effort '{effort}' (kind '{kind_name}')\n",
                kind.harness
            ),
        ));
    }
    if harness == crate::harness::Harness::Opencode {
        match cfg.opencode_model_variants(&kind.model) {
            Some(allowed) => {
                if !allowed.contains(&effort) {
                    return Err((
                        1,
                        format!(
                            "error: harness 'opencode' model '{}' does not take effort '{effort}' (kind '{kind_name}')\n",
                            kind.model
                        ),
                    ));
                }
            }
            None => {
                return Err((
                    1,
                    format!(
                        "error: opencode model '{}' is not in allowlist (fix: add model and variants to team.toml [opencode_models]) (kind '{kind_name}')\n",
                        kind.model
                    ),
                ));
            }
        }
    }
    Ok(Launch {
        harness: kind.harness.clone(),
        model: kind.model.clone(),
        effort: effort.to_string(),
    })
}

/// omp tools a reviewer keeps: no `edit`/`write`/`notebook`/`python`.
pub const OMP_REVIEW_TOOLS: &str = "read,bash,grep,glob";

/// Bash commands a claude reviewer needs to gather evidence. `dontAsk` mode
/// auto-denies every call that would prompt, and the built-in read-only set
/// does not cover a compound `cd <worktree> && git diff`, an unquoted glob
/// beside a write-capable command, or a command the read-only analysis cannot
/// parse — so each read-only prefix is named here. `:*` is Claude Code's
/// trailing wildcard (`Bash(git diff:*)` matches `git diff --stat main`).
pub const CLAUDE_REVIEW_BASH_ALLOW: &[&str] = &[
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(git status:*)",
    "Bash(git rev-parse:*)",
    "Bash(git merge-base:*)",
    "Bash(git grep:*)",
    "Bash(cargo test:*)",
    "Bash(cargo build:*)",
    "Bash(cargo clippy:*)",
    "Bash(cargo fmt --check:*)",
    "Bash(rg:*)",
    "Bash(grep:*)",
    "Bash(cat:*)",
    "Bash(sed -n:*)",
    "Bash(ls:*)",
    // `cd <review worktree> && git diff` prompts on the `cd` alone when it
    // changes directory, even though both halves are read-only.
    "Bash(cd:*)",
];

/// Git commands that write the worktree or the repo. Deny rules outrank allow
/// rules and hold in every mode, so a claude reviewer cannot rewrite the code
/// or the history it is judging even if the permission mode ever changes.
pub const CLAUDE_REVIEW_BASH_DENY: &[&str] = &[
    "Bash(git commit:*)",
    "Bash(git push:*)",
    "Bash(git add:*)",
    "Bash(git rm:*)",
    "Bash(git mv:*)",
    "Bash(git checkout:*)",
    "Bash(git switch:*)",
    "Bash(git restore:*)",
    "Bash(git reset:*)",
    "Bash(git clean:*)",
    "Bash(git stash:*)",
    "Bash(git apply:*)",
    "Bash(git merge:*)",
    "Bash(git rebase:*)",
    "Bash(git cherry-pick:*)",
    "Bash(git revert:*)",
    // `git log/show/diff --output=<file>` writes a file through an allowed
    // read-only prefix.
    "Bash(git *--output*)",
];

/// The worktree family a reviewer may read: worktrees sit side by side in
/// `<repo>/.wt/<name>`, so the sibling directory holds the builder's branch.
/// Falls back to the worktree itself when the caller's path is not `<…>/.wt/<name>`.
pub fn review_read_root(wt: &str) -> String {
    Path::new(wt)
        .parent()
        .filter(|p| p.file_name().is_some_and(|n| n == ".wt"))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| wt.to_string())
}

/// Build settings JSON for Claude Code containing per-worker hooks for liveness
/// state (`runs/.<name>.state`), and reviewer permissions if `read_root` is provided.
pub fn claude_settings(name: &str, runs_dir: &str, read_root: Option<&str>) -> serde_json::Value {
    let state_file = format!("{runs_dir}/.{name}.state");
    let state_tmp = format!("{runs_dir}/.{name}.state.tmp");
    // A blocked state carries a reason token after the timestamp
    // (`blocked <ts> <reason>`); `status` maps it to a fixed label (#179) and
    // `HookState::parse` still reads only the first two tokens.
    let tool_re =
        |tool: &str| format!("\"\\\"tool_name\\\"[[:space:]]*:[[:space:]]*\\\"{tool}\\\"\"");
    let pre_tool_cmd = format!(
        "sh -c 'p=$(cat); s=\"working\"; r=\"\"; if printf %s \"$p\" | grep -Eq {ask}; then s=\"blocked\"; r=\" question\"; elif printf %s \"$p\" | grep -Eq {plan}; then s=\"blocked\"; r=\" plan\"; fi; echo \"$s $(date +%s)$r\" > \"{state_tmp}\" && mv \"{state_tmp}\" \"{state_file}\"'",
        ask = tool_re("AskUserQuestion"),
        plan = tool_re("ExitPlanMode"),
    );
    let stop_cmd = format!(
        "sh -c 'echo \"idle $(date +%s)\" > \"{state_tmp}\" && mv \"{state_tmp}\" \"{state_file}\"'"
    );
    let notif_cmd = format!(
        "sh -c 'p=$(cat); r=\"\"; if printf %s \"$p\" | grep -q permission_prompt; then r=\" permission\"; elif printf %s \"$p\" | grep -Eiq \"not logged in|/login|log in|sign in|signed out|auth_required\"; then r=\" signed-out\"; fi; echo \"blocked $(date +%s)$r\" > \"{state_tmp}\" && mv \"{state_tmp}\" \"{state_file}\"'"
    );

    let hooks = serde_json::json!({
        "PreToolUse": [{ "hooks": [{ "type": "command", "command": pre_tool_cmd }] }],
        "Stop": [{ "hooks": [{ "type": "command", "command": stop_cmd }] }],
        "Notification": [{ "hooks": [{ "type": "command", "command": notif_cmd }] }]
    });

    if let Some(read_root) = read_root {
        let runs_clean = runs_dir.trim_start_matches('/');
        let read_clean = read_root.trim_start_matches('/');
        let mut allow = vec![
            format!("Edit(//{runs_clean}/**)"),
            "Bash(mv *runs/.*.tmp *runs/*.md)".to_string(),
        ];
        allow.extend(CLAUDE_REVIEW_BASH_ALLOW.iter().map(|r| r.to_string()));
        let mut deny = vec![format!("Edit(//{read_clean}/**)")];
        deny.extend(CLAUDE_REVIEW_BASH_DENY.iter().map(|r| r.to_string()));
        serde_json::json!({
            "permissions": { "allow": allow, "deny": deny },
            "hooks": hooks
        })
    } else {
        serde_json::json!({
            "hooks": hooks
        })
    }
}

/// One attempt's `herdr` invocation: the pane-split envs and the args after
/// `herdr agent start <name> --kind <harness> --pane <p> --timeout <ms>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCmd {
    pub split_envs: Vec<(String, String)>,
    pub agent_args: Vec<String>,
}

/// Build the exact launch command for a resolved kind. opencode has no `--model`
/// on the interactive TUI (`opencode --help` flags are --standalone/--server/
/// --auto/--continue/--session/--prompt), so its model+effort travel as the
/// config `model` object `{"providerID": "...", "model": "...", "variant": "..."}`
/// in OPENCODE_CONFIG_CONTENT in the pane env.
/// omp and pi take `--model`/`--thinking`, claude takes `--model`/`--effort`.
pub fn build_launch_cmd(
    launch: &Launch,
    name: &str,
    rules_file: &str,
    runs_dir: &str,
    wt: &str,
    review: bool,
) -> Result<LaunchCmd, (i32, String)> {
    let mut split_envs: Vec<(String, String)> = Vec::new();

    let agent_args: Vec<String> = match launch.harness.as_str() {
        "opencode" => {
            let (provider_id, model_id) = match launch.model.split_once('/') {
                Some((p, m)) => (p, m),
                None => ("opencode-go", launch.model.as_str()),
            };
            let cfg = format!(
                "{{\"model\":{{\"providerID\":\"{}\",\"model\":\"{}\",\"variant\":\"{}\"}},\"instructions\":[\"{}\"]}}",
                provider_id, model_id, launch.effort, rules_file
            );
            split_envs.push(("OPENCODE_CONFIG_CONTENT".to_string(), cfg));
            let state_dir = format!("{runs_dir}/.opencode-state/{name}");
            split_envs.push(("XDG_STATE_HOME".to_string(), state_dir));
            vec![
                "--".to_string(),
                "--standalone".to_string(),
                "--auto".to_string(),
            ]
        }
        "omp" => {
            let mut args = vec![
                "--".to_string(),
                "--model".to_string(),
                launch.model.clone(),
                "--thinking".to_string(),
                launch.effort.clone(),
                "--auto-approve".to_string(),
            ];
            // Review: read tools + bash only, no edit/write (`omp --help`).
            // Detection (`worker::prompt::readonly`) is the guarantee.
            if review {
                args.push(format!("--tools={OMP_REVIEW_TOOLS}"));
            }
            args.push("--append-system-prompt".to_string());
            args.push(rules_file.to_string());
            args
        }
        "pi" => {
            // Isolated: only herdr's state extension loads, so personal
            // subagent/memory extensions cannot spawn outside the grid.
            let home = std::env::var("HOME").unwrap_or_default();
            let mut args = vec![
                "--".to_string(),
                "--model".to_string(),
                launch.model.clone(),
                "--thinking".to_string(),
                launch.effort.clone(),
                "--append-system-prompt".to_string(),
                rules_file.to_string(),
                "--session-dir".to_string(),
                format!("{runs_dir}/.pi-sessions/{name}"),
                "--no-extensions".to_string(),
                "-e".to_string(),
                format!("{home}/.pi/agent/extensions/herdr-agent-state.ts"),
                "--no-approve".to_string(),
            ];
            if review {
                args.push("--tools".to_string());
                args.push("read,grep,find,ls,bash".to_string());
            }
            args
        }
        "claude" => {
            let claude_model = if launch.model.starts_with("claude-") {
                launch.model.clone()
            } else {
                format!("claude-{}", launch.model.replace('.', "-"))
            };
            let mut args = vec![
                "--".to_string(),
                "--model".to_string(),
                claude_model,
                "--effort".to_string(),
                launch.effort.clone(),
            ];
            if review {
                let read_root = review_read_root(wt);
                args.push("--permission-mode".to_string());
                args.push("dontAsk".to_string());
                // Working directories: file reads inside them need no approval,
                // and `dontAsk` denies every read that would ask.
                args.push("--add-dir".to_string());
                args.push(runs_dir.to_string());
                args.push(read_root.clone());
                args.push("--settings".to_string());
                args.push(claude_settings(name, runs_dir, Some(&read_root)).to_string());
            } else {
                args.push("--settings".to_string());
                args.push(claude_settings(name, runs_dir, None).to_string());
            }
            args.push("--append-system-prompt-file".to_string());
            args.push(rules_file.to_string());
            args
        }
        other => return Err((1, format!("error: unsupported harness '{other}'\n"))),
    };

    Ok(LaunchCmd {
        split_envs,
        agent_args,
    })
}

/// Is Claude Code's workspace-trust dialog still on screen? Its wording varies
/// between versions; every variant contains the word "trust" ("Quick safety
/// check: is this a project you created or one you trust?" / "Yes, I trust this
/// folder"), and it is word-wrapped to fit the pane, so match that one word
/// rather than a phrase. The look is deliberately sensitive: answering a dialog
/// that is not there costs a harmless Down and Enter on an empty prompt box,
/// missing one leaves the worker stalled.
pub fn claude_trust_dialog_up<H: HerdrOps>(herdr: &H, pane: &str) -> bool {
    herdr
        .pane_read(pane, "visible", 30)
        .map(|text| text.contains("trust"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::claude_settings;
    use crate::status::BlockReason;
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Run one generated hook with `payload` on stdin; return the state line it wrote.
    fn run_hook(event: &str, payload: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "quaqua-hook-{}-{}",
            std::process::id(),
            &crate::hash::md5_hex(format!("{event}{payload}").as_bytes())[..12]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let s = claude_settings("w", dir.to_str().unwrap(), None);
        let cmd = s["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success(), "{cmd}");
        let out = std::fs::read_to_string(dir.join(".w.state")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    fn reason(line: &str) -> Option<BlockReason> {
        let mut p = line.split_whitespace();
        (p.next() == Some("blocked")).then(|| BlockReason::from_token(p.nth(1)))
    }

    #[test]
    fn liveness_hooks_write_block_reason_token() {
        let cases = [
            (
                "PreToolUse",
                r#"{"tool_name": "AskUserQuestion"}"#,
                Some(BlockReason::QuestionForYou),
            ),
            (
                "PreToolUse",
                r#"{"tool_name":"ExitPlanMode"}"#,
                Some(BlockReason::PlanToApprove),
            ),
            ("PreToolUse", r#"{"tool_name":"Bash"}"#, None),
            (
                "Notification",
                r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"}"#,
                Some(BlockReason::AsksPermission),
            ),
            (
                "Notification",
                r#"{"hook_event_name":"Notification","message":"Not logged in. Please run /login"}"#,
                Some(BlockReason::SignedOut),
            ),
            (
                "Notification",
                r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","message":"Claude is waiting for your input"}"#,
                Some(BlockReason::NeedsInput),
            ),
        ];
        for (event, payload, want) in cases {
            let line = run_hook(event, payload);
            // Backward compatible: state + timestamp still parse.
            let hs = crate::liveness::HookState::parse(&line).unwrap();
            assert!(hs.timestamp_sec > 0, "{line}");
            assert_eq!(reason(&line), want, "{event} {payload} -> {line}");
        }
    }
}
