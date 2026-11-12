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
                if !allowed.contains(effort) {
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

/// Pure helper for resolving the path to the `omp-worker-state.ts` extension.
pub fn omp_worker_state_hook_path_from_env(
    runs_dir: &str,
    env_override: Option<&str>,
    workspace_root: &Path,
) -> String {
    if let Some(p) = env_override {
        if !p.is_empty() {
            return p.to_string();
        }
    }
    if let Some(parent) = Path::new(runs_dir).parent() {
        let candidate = parent.join("scripts/hooks/omp-worker-state.ts");
        if candidate.is_file() {
            return candidate.to_string_lossy().to_string();
        }
    }
    let ws_hook = workspace_root.join("scripts/hooks/omp-worker-state.ts");
    if ws_hook.is_file() {
        return ws_hook.to_string_lossy().to_string();
    }
    ws_hook.to_string_lossy().to_string()
}

/// Resolve the path to the `omp-worker-state.ts` extension.
pub fn omp_worker_state_hook_path(runs_dir: &str) -> String {
    omp_worker_state_hook_path_from_env(
        runs_dir,
        std::env::var("QUAQUA_OMP_STATE_HOOK").ok().as_deref(),
        &crate::workspace::workspace_root(),
    )
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

/// Write a worker's Claude settings and return the file path.
///
/// The settings travel as a file, never inline: herdr types the whole launch
/// command into the pane's terminal, and a canonical-mode tty (macOS) refuses a
/// line over 1024 bytes (`agent_start_input_failed`) before Claude Code ever
/// runs — a review's inline settings alone are ~2 KB.
pub fn write_claude_settings(
    name: &str,
    runs_dir: &str,
    read_root: Option<&str>,
) -> Result<String, (i32, String)> {
    let path = crate::state::WorkerMetadata::settings_path(Path::new(runs_dir), name);
    let body = serde_json::to_string_pretty(&claude_settings(name, runs_dir, read_root))
        .map_err(|e| (1, format!("error: cannot render claude settings for '{name}': {e}\n")))?;
    std::fs::write(&path, body).map_err(|e| {
        (
            1,
            format!(
                "error: cannot write claude settings for '{name}' to {} (fix: create the runs directory): {e}\n",
                path.display()
            ),
        )
    })?;
    Ok(path.to_string_lossy().to_string())
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
/// omp and pi take `--model`/`--thinking`, claude takes `--model`/`--effort`,
/// agy takes `--model`/`--effort` and reads its rules from an `AGENTS.md` in a
/// per-name `--add-dir` directory.
///
/// Building a claude command writes `runs/.<name>.settings.json` (the caller
/// must have created `runs_dir`) and names it on `--settings`, so the launch
/// line stays one short typed line.
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
            let state_file = format!("{runs_dir}/.{name}.state");
            let error_file = format!("{runs_dir}/.{name}.error");
            split_envs.push(("QUAQUA_STATE_FILE".to_string(), state_file));
            split_envs.push(("QUAQUA_ERROR_FILE".to_string(), error_file));

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
            let worker_state_ext = omp_worker_state_hook_path(runs_dir);
            args.push("-e".to_string());
            args.push(worker_state_ext);
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
        "agy" => {
            // agy reads `AGENTS.md` from the workspace root and from every
            // `--add-dir` directory, and has no system-prompt flag, so the
            // worker rules land as a context file in a per-name dir instead of
            // on the typed launch line (see the claude settings note above).
            let rules_dir = format!("{runs_dir}/.agy/{name}");
            std::fs::create_dir_all(&rules_dir).map_err(|e| {
                (
                    1,
                    format!("error: cannot create agy rules dir {rules_dir}: {e}\n"),
                )
            })?;
            let rules_copy = format!("{rules_dir}/AGENTS.md");
            let rules_body = std::fs::read_to_string(rules_file).map_err(|e| {
                (
                    1,
                    format!("error: cannot read worker rules {rules_file}: {e}\n"),
                )
            })?;
            std::fs::write(&rules_copy, rules_body).map_err(|e| {
                (
                    1,
                    format!("error: cannot write agy rules file {rules_copy}: {e}\n"),
                )
            })?;
            // agy asks "Do you trust the contents of this project?" on the
            // first run in a directory and records the answer per path, so
            // every new worktree would stall on the dialog with the brief
            // typed into it. A session-only `trustedWorkspaces` entry for the
            // worktree skips it; the file keeps the JSON off the typed launch
            // line (herdr types the whole command into the pane).
            let settings_file = format!("{rules_dir}/settings.json");
            let settings = serde_json::json!({ "trustedWorkspaces": [wt] }).to_string();
            std::fs::write(&settings_file, settings).map_err(|e| {
                (
                    1,
                    format!("error: cannot write agy settings {settings_file}: {e}\n"),
                )
            })?;
            vec![
                "--".to_string(),
                "--model".to_string(),
                launch.model.clone(),
                "--effort".to_string(),
                launch.effort.clone(),
                "--dangerously-skip-permissions".to_string(),
                "--settings".to_string(),
                settings_file,
                "--add-dir".to_string(),
                rules_dir,
                // The report lives outside the worktree; claude gets the same.
                "--add-dir".to_string(),
                runs_dir.to_string(),
            ]
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
                args.push(write_claude_settings(name, runs_dir, Some(&read_root))?);
            } else {
                args.push("--settings".to_string());
                args.push(write_claude_settings(name, runs_dir, None)?);
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

    #[test]
    fn omp_launch_cmd_carries_state_and_error_envs_and_extensions() {
        let launch = super::Launch {
            harness: "omp".to_string(),
            model: "google-antigravity/gemini-3.8-flash".to_string(),
            effort: "high".to_string(),
        };
        let cmd = super::build_launch_cmd(
            &launch,
            "sude-w1",
            "/path/to/rules.md",
            "/workspace/runs",
            "/workspace/.wt/sude-w1",
            false,
        )
        .unwrap();

        assert_eq!(cmd.split_envs.len(), 2);
        assert_eq!(
            cmd.split_envs[0],
            (
                "QUAQUA_STATE_FILE".to_string(),
                "/workspace/runs/.sude-w1.state".to_string()
            )
        );
        assert_eq!(
            cmd.split_envs[1],
            (
                "QUAQUA_ERROR_FILE".to_string(),
                "/workspace/runs/.sude-w1.error".to_string()
            )
        );

        // Finding 1 [major]: No --no-extensions, discovery remains enabled, exactly one -e
        assert!(!cmd.agent_args.iter().any(|a| a == "--no-extensions"));
        assert!(!cmd.agent_args.iter().any(|a| a.contains("herdr-omp-agent-state.ts")));
        let ext_indices: Vec<usize> = cmd
            .agent_args
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "-e")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(ext_indices.len(), 1, "must pass exactly one -e for worker state extension");
        let worker_ext = super::omp_worker_state_hook_path("/workspace/runs");
        assert_eq!(cmd.agent_args[ext_indices[0] + 1], worker_ext);

        // Review mode includes OMP_REVIEW_TOOLS
        let cmd_rev = super::build_launch_cmd(
            &launch,
            "sude-rev",
            "/path/to/rules.md",
            "/workspace/runs",
            "/workspace/.wt/sude-rev",
            true,
        )
        .unwrap();
        assert!(cmd_rev
            .agent_args
            .iter()
            .any(|a| a == &format!("--tools={}", super::OMP_REVIEW_TOOLS)));
    }

    #[test]
    fn omp_worker_state_hook_path_pure_resolver_respects_override() {
        let p = super::omp_worker_state_hook_path_from_env(
            "/workspace/runs",
            Some("/custom/omp-state.ts"),
            std::path::Path::new("/dummy/workspace"),
        );
        assert_eq!(p, "/custom/omp-state.ts");
    }

    #[test]
    fn omp_hook_path_ignores_cwd_templates_and_falls_back_to_workspace() {
        // Finding 3 [minor]: Does not check cwd-relative templates/ directory;
        // falls back to <workspace_root>/scripts/hooks/omp-worker-state.ts.
        let ws = std::path::Path::new("/nonexistent/ws");
        let p = super::omp_worker_state_hook_path_from_env("/nonexistent/runs", None, ws);
        assert_eq!(p, "/nonexistent/ws/scripts/hooks/omp-worker-state.ts");
    }

    /// Length of the line herdr types into the pane: the harness binary, then
    /// every arg, shell-quoted wherever the shell would need a quote.
    fn typed_line_len(harness: &str, args: &[String]) -> usize {
        std::iter::once(harness.to_string())
            .chain(args.iter().cloned())
            .map(|a| {
                let plain = !a.is_empty()
                    && a.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.:/=,@+".contains(&b));
                if plain {
                    a
                } else {
                    format!("'{}'", a.replace('\'', r"'\''"))
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
            .len()
    }

    /// herdr types the whole launch command into the pane's terminal, and a
    /// canonical-mode tty (macOS) refuses a line over 1024 bytes with
    /// `agent_start_input_failed` before the agent ever runs. Every claude
    /// launch line must stay under that cap for the longest name a spawn allows
    /// (20 chars) and a real runs path.
    #[test]
    fn claude_launch_line_fits_one_typed_terminal_line() {
        const HERDR_TYPED_LINE_LIMIT: usize = 1024;
        let runs = std::env::temp_dir().join("quaqua-cl-line/runs");
        std::fs::create_dir_all(&runs).unwrap();
        let runs = runs.to_string_lossy().to_string();
        // `is_valid_name` caps a spawn name at 20 chars (`worker/spawn.rs`).
        let name = "suhuynh-reviewer-w1x";
        assert_eq!(name.len(), 20, "worst-case spawn name");
        let launch = super::Launch {
            harness: "claude".to_string(),
            model: "opus-5.5".to_string(),
            effort: "high".to_string(),
        };
        for review in [false, true] {
            let cmd = super::build_launch_cmd(
                &launch,
                name,
                &format!("{runs}/{name}.rules.md"),
                &runs,
                &format!("/Users/someone/Lab/.wt/{name}"),
                review,
            )
            .unwrap();
            let len = typed_line_len("claude", &cmd.agent_args);
            assert!(
                len < HERDR_TYPED_LINE_LIMIT,
                "review={review}: claude launch line is {len} bytes, over herdr's \
                 {HERDR_TYPED_LINE_LIMIT}-byte typed-line limit: {}",
                cmd.agent_args.join(" ")
            );
        }
    }

    /// The line has to stay short *and* still hand claude its settings: the
    /// file named after `--settings` must exist and carry this worker's
    /// liveness hooks (and, for a review, the read-only permission boundary),
    /// because claude loads hooks and permissions from that file.
    #[test]
    fn claude_launch_hands_claude_a_written_settings_file() {
        let dir = std::env::temp_dir().join(format!(
            "quaqua-cl-settings-{}-{}",
            std::process::id(),
            "suhuynh-rev"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let runs = dir.to_string_lossy().to_string();
        let launch = super::Launch {
            harness: "claude".to_string(),
            model: "opus-5.5".to_string(),
            effort: "high".to_string(),
        };
        for review in [false, true] {
            let cmd = super::build_launch_cmd(
                &launch,
                "suhuynh-rev",
                "/r/x.rules.md",
                &runs,
                "/r/.wt/suhuynh-rev",
                review,
            )
            .unwrap();
            let at = cmd
                .agent_args
                .iter()
                .position(|a| a == "--settings")
                .expect("claude takes its settings through --settings");
            let body = std::fs::read_to_string(&cmd.agent_args[at + 1])
                .unwrap_or_else(|e| panic!("review={review}: --settings must name a file claude can read: {e}"));
            assert!(
                body.contains(&format!("{runs}/.suhuynh-rev.state")),
                "review={review}: hooks must record this worker's state: {body}"
            );
            assert!(body.contains("\"PreToolUse\""), "review={review}: {body}");
            assert_eq!(
                body.contains("\"deny\""),
                review,
                "review={review}: only a review carries the write-denying boundary: {body}"
            );
        }
    }

    #[test]
    fn omp_worker_state_tokens_parse_cleanly() {
        use crate::liveness::{HookState, WorkerState};
        use crate::status::BlockReason;

        let working = "working 1730500000";
        let hs = HookState::parse(working).unwrap();
        assert_eq!(hs.state, WorkerState::Working);
        assert_eq!(hs.timestamp_sec, 1730500000);

        let idle = "idle 1730500001";
        let hs = HookState::parse(idle).unwrap();
        assert_eq!(hs.state, WorkerState::Idle);
        assert_eq!(hs.timestamp_sec, 1730500001);

        let test_cases = [
            ("blocked 1730500002 permission", BlockReason::AsksPermission),
            ("blocked 1730500003 plan", BlockReason::PlanToApprove),
            ("blocked 1730500004 question", BlockReason::QuestionForYou),
            ("blocked 1730500005 signed-out", BlockReason::SignedOut),
        ];
        for (line, expected_reason) in test_cases {
            let hs = HookState::parse(line).unwrap();
            assert_eq!(hs.state, WorkerState::Blocked);
            let mut parts = line.split_whitespace();
            assert_eq!(parts.next(), Some("blocked"));
            let token = parts.nth(1);
            assert_eq!(BlockReason::from_token(token), expected_reason);
        }
    }
}
