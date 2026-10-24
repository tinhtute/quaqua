//! Worker spawn logic (`quaqua worker spawn [--base <ref>] [--review] <name> <kind> <repo-path>`).

use crate::cap::count_owner_slots;
use crate::config::{Config, Kind};
use crate::git::GitOps;
use crate::herdr::HerdrOps;
pub use crate::launch::{
    build_launch_cmd, claude_trust_dialog_up, resolve_launch, Launch, LaunchCmd,
};
use crate::state::WorkerMetadata;
use crate::worker::cleanup::run_grid_hook;
pub use crate::worker::worktree::{
    apply_push_block, canonicalize_repo_path, prepare_worktree, remove_worktree, resolve_paths,
    resolve_worktree_path, setup_worktree, validate_base_ref,
};
pub use crate::layout::SpawnOrigin;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Output;

/// Credential env vars a worker must never inherit from the spawning shell.
/// Provider API keys are deliberately absent: the harness needs its own auth
/// to run. `SSH_AUTH_SOCK` stays too (read-only ssh fetches); pushes are
/// stopped by the pre-push hook instead.
const CREDENTIAL_ENV_VARS: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITHUB_PAT",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
];

pub const USAGE: &str = "\
# Usage: quaqua worker spawn [--base <ref>] [--review] [--approve-kind <kind>] [--purpose <purpose>] [--solo \"<reason>\"] <name> [<kind>] <repo-path>
#   name = <kind>-<task>, ascii [a-z0-9-], <=20 chars, not ending in -lead
#          (*-lead is the project Lead convention — quaqua lead spawn <slug>)
#   --base <ref>  start branch quaqua/<name> at <ref> instead of <repo> HEAD
#   --review      any kind with can_review = true: repo read-only, $HOME/quaqua/runs writable
#   --purpose <p> resolve kind from [purposes] in team.toml
#   --approve-kind <kind>  approve a spawn when kinds.<kind>.spawn = \"approve\" (repeatable)
#   --solo \"<reason>\"  spawn on a cos-owned project without a Lead: needs a reason of
#                 at least 10 chars, printed and recorded in memory/<today>.md.
#                 The gate blocks at >= 2 topology signals (live build workers,
#                 plan phases_left, multi-day memory, brief budget) — AGENTS.md '## Roles (SLP)'";

pub struct SpawnOptions {
    pub base: Option<String>,
    pub review: bool,
    pub name: String,
    pub kind: Option<String>,
    pub purpose: Option<String>,
    pub repo_path: PathBuf,
    /// Kinds the Principal approved with `--approve-kind`.
    pub approve_kinds: Vec<String>,
    /// Reason given with `--solo`: spawn without a Lead, recorded in memory/.
    pub solo: Option<String>,
}

pub fn parse_spawn_args(args: &[String]) -> Result<SpawnOptions, (i32, String)> {
    let mut base: Option<String> = None;
    let mut review = false;
    let mut purpose: Option<String> = None;
    let mut approve_kinds: Vec<String> = Vec::new();
    let mut solo: Option<String> = None;
    let mut pos = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--review" {
            review = true;
            i += 1;
        } else if arg == "--solo" {
            if i + 1 >= args.len() {
                return Err((1, format!("{}\n", USAGE)));
            }
            let val = &args[i + 1];
            crate::worker::leadgate::validate_solo(val)
                .map_err(|(code, msg)| (code, format!("{msg}{USAGE}\n")))?;
            solo = Some(val.clone());
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--solo=") {
            crate::worker::leadgate::validate_solo(val)
                .map_err(|(code, msg)| (code, format!("{msg}{USAGE}\n")))?;
            solo = Some(val.to_string());
            i += 1;
        } else if arg == "--approve-kind" {
            if i + 1 >= args.len() {
                return Err((1, format!("{}\n", USAGE)));
            }
            let val = &args[i + 1];
            if val.is_empty() || val.starts_with('-') {
                return Err((1, format!("{}\n", USAGE)));
            }
            approve_kinds.push(val.clone());
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--approve-kind=") {
            if val.is_empty() || val.starts_with('-') {
                return Err((1, format!("{}\n", USAGE)));
            }
            approve_kinds.push(val.to_string());
            i += 1;
        } else if arg == "--base" {
            if i + 1 >= args.len() {
                return Err((1, format!("{}\n", USAGE)));
            }
            let val = &args[i + 1];
            if val.is_empty() || val.starts_with('-') {
                return Err((1, format!("{}\n", USAGE)));
            }
            base = Some(val.clone());
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--base=") {
            if val.is_empty() || val.starts_with('-') {
                return Err((1, format!("{}\n", USAGE)));
            }
            base = Some(val.to_string());
            i += 1;
        } else if arg == "--purpose" {
            if i + 1 >= args.len() {
                return Err((1, format!("{}\n", USAGE)));
            }
            let val = &args[i + 1];
            if val.is_empty() || val.starts_with('-') {
                return Err((1, format!("{}\n", USAGE)));
            }
            purpose = Some(val.clone());
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--purpose=") {
            if val.is_empty() || val.starts_with('-') {
                return Err((1, format!("{}\n", USAGE)));
            }
            purpose = Some(val.to_string());
            i += 1;
        } else if arg.starts_with('-') {
            return Err((1, format!("{}\n", USAGE)));
        } else {
            pos.push(arg.clone());
            i += 1;
        }
    }

    let (name, kind, repo_path) = if pos.len() == 2 && purpose.is_some() {
        (pos[0].clone(), None, PathBuf::from(&pos[1]))
    } else if pos.len() >= 3 {
        (pos[0].clone(), Some(pos[1].clone()), PathBuf::from(&pos[2]))
    } else {
        return Err((1, format!("{}\n", USAGE)));
    };

    Ok(SpawnOptions {
        base,
        review,
        name,
        kind,
        purpose,
        repo_path,
        approve_kinds,
        solo,
    })
}

fn is_valid_name(name: &str, kind: &str) -> bool {
    let prefix = format!("{kind}-");
    if !name.starts_with(&prefix) {
        return false;
    }
    if name.len() > 20 || name.ends_with("-lead") {
        return false;
    }
    let rest = &name[prefix.len()..];
    if rest.is_empty() {
        return false;
    }
    rest.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}
/// A short, stable slug for why an agent start failed: herdr's `error.code`
/// when the output carries one (`agent_not_ready`, auth/quota codes), else
/// `start-failed`.
pub fn start_failure_reason(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let text = if stdout.trim().is_empty() {
        String::from_utf8_lossy(&out.stderr).into_owned()
    } else {
        stdout.into_owned()
    };
    if let Ok(j) = serde_json::from_str::<Value>(&text) {
        if let Some(code) = j
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_str())
        {
            if !code.is_empty() {
                return code.to_string();
            }
        }
    }
    "start-failed".to_string()
}

/// Resolve a kind's configured fallback, if any. Returns `Ok(None)` if no fallback
/// is configured, `Ok(Some((fallback_name, &Kind)))` if valid, or an error if
/// the configured fallback is not a known kind.
pub fn fallback_kind<'a>(
    cfg: &'a Config,
    kind: &'a Kind,
    kind_name: &str,
) -> Result<Option<(&'a str, &'a Kind)>, (i32, String)> {
    match kind.fallback.as_deref() {
        None => Ok(None),
        Some(fb) => {
            let fb_kind = cfg.kinds.get(fb).ok_or_else(|| {
                (
                    1,
                    format!("error: kind '{kind_name}': fallback '{fb}' is not a kind\n"),
                )
            })?;
            Ok(Some((fb, fb_kind)))
        }
    }
}

/// Default workers-tab label for a spawn. A project Lead's workers belong to its
/// project tab — owner `<slug>-lead` → `<slug>-workers`, the exact tab the Lead's
/// spawn instructions name — not to a tab derived from the repo dir name: one
/// checkout (`quaqua`) hosts several projects (`quaqua-rs`, `quaqua-grid`), and
/// a repo-derived label splits one owner's workers across tabs
/// (runs/tieude-testlow.grid vs runs/tieude-spawneff.grid). Any other owner
/// (the CoS) keeps the repo-derived label. Explicit GRID_TAB wins before this
/// is reached.
pub fn default_grid_tab(owner: &str, repo_name: &str) -> String {
    match owner.strip_suffix("-lead") {
        Some(slug) if !slug.is_empty() => format!("{slug}-workers"),
        _ => format!("{repo_name}-workers"),
    }
}

/// The owner of the pane this process runs in, when herdr can still see it:
/// `grid_owner` of a pane that is not itself a worker.
fn resolve_owner_from_tokens(tokens: Option<&std::collections::HashMap<String, String>>) -> String {
    let env_owner = std::env::var("QUAQUA_OWNER")
        .ok()
        .filter(|o| !o.is_empty())
        .unwrap_or_else(|| "cos".to_string());
    let pane_owner = tokens.and_then(|t| {
        if t.get("grid_role").map(|s| s.as_str()) == Some("worker") {
            return None;
        }
        t.get("grid_owner").map(|o| o.trim().to_string()).filter(|o| !o.is_empty())
    });
    match pane_owner {
        Some(pane)
            if crate::cap::is_lead_owner(&pane) && !crate::cap::is_lead_owner(&env_owner) =>
        {
            pane
        }
        _ => env_owner,
    }
}

/// Resolve the owner a spawn belongs to.
///
/// `$QUAQUA_OWNER` is the normal source, but it lives in the *shell*: a herdr
/// restart restores panes with the daemon's environment, so a Lead's shell can
/// say `cos` while its pane metadata still says `<slug>-lead`. The pane is the
/// durable record, so a tagged non-worker pane wins — except over an explicit
/// project-Lead owner in the environment (`QUAQUA_OWNER=<slug>-lead`, e.g. a
/// pipeline spawning on behalf of its Lead): a project owner is specific, `cos`
/// is the fallback. Workers spawned by such a Lead were tagged `grid_owner=cos`
/// and landed in `quaqua-workers` instead of the project's tab, mixing projects.
pub fn resolve_owner<H: HerdrOps>(herdr: &H) -> String {
    let tokens = crate::identity::caller_pane_tokens(herdr);
    resolve_owner_from_tokens(tokens.as_ref())
}

/// Pane env for a worker: the launch env plus blank copies of every credential
/// name, so the herdr daemon's inherited value cannot reach the agent (`pane
/// split` has no `--unset-env`; an empty value carries no secret).
fn worker_split_envs<'a>(cmd: &'a LaunchCmd) -> Vec<(&'a str, &'a str)> {
    let mut envs: Vec<(&'a str, &'a str)> = cmd
        .split_envs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    for name in CREDENTIAL_ENV_VARS {
        envs.push((*name, ""));
    }
    envs
}

pub fn spawn_worker<G: GitOps, H: HerdrOps>(
    opts: SpawnOptions,
    cfg: &Config,
    git: &G,
    herdr: &H,
    origin: SpawnOrigin,
) -> Result<(), (i32, String)> {
    let runs_dir = crate::worker::cleanup::default_runs_dir();
    let name = opts.name.clone();
    let repo_name = opts
        .repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(String::from);

    let res = spawn_worker_inner(opts, cfg, git, herdr, &runs_dir, origin);

    match &res {
        Ok((caller, owner)) => {
            crate::bus::record_event_safely_in_runs(
                &runs_dir,
                &crate::bus::FarmEvent::new(
                    "spawned",
                    Some(caller.clone()),
                    Some(name),
                    Some(owner.clone()),
                    repo_name,
                ),
            );
            Ok(())
        }
        Err((code, msg)) => {
            let payload = if msg.trim().is_empty() {
                "{}".to_string()
            } else {
                serde_json::to_string(&serde_json::json!({ "error": msg.trim() }))
                    .unwrap_or_else(|_| "{}".to_string())
            };
            crate::bus::record_event_safely_in_runs(
                &runs_dir,
                &crate::bus::FarmEvent::new(
                    "spawn_failed",
                    None::<String>,
                    if name.is_empty() { None } else { Some(name) },
                    None::<String>,
                    repo_name,
                )
                .with_payload_json(payload),
            );
            Err((*code, msg.clone()))
        }
    }
}

fn spawn_worker_inner<G: GitOps, H: HerdrOps>(
    mut opts: SpawnOptions,
    cfg: &Config,
    git: &G,
    herdr: &H,
    runs_dir: &std::path::Path,
    origin: SpawnOrigin,
) -> Result<(String, String), (i32, String)> {
    let resolved_kind = match (&opts.purpose, &opts.kind) {
        (Some(p), Some(pos)) => {
            let resolved = crate::config::resolve_purpose(cfg, p)
                .map_err(|e| (1, format!("error: {e}\n")))?;
            if pos != resolved {
                return Err((
                    1,
                    format!(
                        "error: --purpose '{p}' resolves to kind '{resolved}', but positional kind is '{pos}'\n"
                    ),
                ));
            }
            resolved.to_string()
        }
        (Some(p), None) => {
            crate::config::resolve_purpose(cfg, p)
                .map_err(|e| (1, format!("error: {e}\n")))?
                .to_string()
        }
        (None, Some(k)) => {
            if cfg.kinds.contains_key(k) {
                k.clone()
            } else if let Ok(resolved) = crate::config::resolve_purpose(cfg, k) {
                resolved.to_string()
            } else {
                k.clone()
            }
        }
        (None, None) => {
            return Err((1, format!("{}\n", USAGE)));
        }
    };
    opts.kind = Some(resolved_kind.clone());
    let kind = resolved_kind;

    let kind_info = cfg.kinds.get(&kind).ok_or_else(|| {
        let line2 = USAGE.lines().next().unwrap_or(USAGE);
        (1, format!("{line2}\n"))
    })?;
    if !is_valid_name(&opts.name, &kind) {
        let lines: Vec<&str> = USAGE.lines().take(2).collect();
        return Err((1, format!("{}\n", lines.join("\n"))));
    }

    // One spawn-policy gate for every spawn path: a kind with
    // `spawn = "approve"` needs its own --approve-kind from the Principal.
    crate::config::check_spawn_policy(&kind, kind_info, &opts.approve_kinds)?;

    if opts.review && !kind_info.can_review {
        eprintln!(
            "error: --review requires a kind with can_review = true in team.toml, not {}",
            kind
        );
        return Err((1, format!("{}\n", USAGE)));
    }

    // team.toml is the only source of harness/model/effort: reject a kind that
    // cannot launch before any worktree or pane exists.
    let primary_launch = resolve_launch(kind_info, &kind, cfg)?;
    let primary_fallback = kind_info.fallback.clone();

    let repo = canonicalize_repo_path(&opts.repo_path);
    let wt = resolve_worktree_path(&repo, &opts.name);

    if let Some(base_ref) = &opts.base {
        validate_base_ref(git, &repo, base_ref)?;
    }

    let caller_tokens = crate::identity::caller_pane_tokens(herdr);
    let owner = resolve_owner_from_tokens(caller_tokens.as_ref());
    let caller = crate::identity::resolve_caller_from_tokens(caller_tokens.as_ref(), runs_dir);
    let _ = std::fs::create_dir_all(runs_dir);
    eprintln!("[identity] worker spawn: name={} caller={caller}", opts.name);

    crate::layout::shadow(
        Some(&caller),
        Some(herdr),
        crate::layout::Verb::Spawn,
        "peer",
        Some(&opts.name),
        origin,
        Some(&owner),
        opts.repo_path.file_name().and_then(|n| n.to_str()),
        runs_dir,
        Some(cfg),
    );

    // Lead gate (AGENTS.md '## Roles (SLP)'): a `cos`-owned project with two or
    // more topology signals needs a Lead before another worker joins it.
    let workspace_root = crate::workspace::workspace_root();
    let pipeline_task = std::env::var("PIPELINE_TASK").ok();
    crate::worker::leadgate::check_spawn(
        crate::worker::leadgate::GateInput {
            root: &workspace_root,
            runs_dir: &runs_dir,
            repo: &repo,
            name: &opts.name,
            brief: None,
            solo: opts.solo.as_deref(),
            pipeline: pipeline_task.as_deref(),
            caller: &caller,
        },
        cfg,
        herdr,
    )?;

    if std::env::var("PIPELINE_TASK").is_err() {
        let slots_used = count_owner_slots(&runs_dir, &owner, Some(&opts.name), herdr, cfg);
        crate::cap::check_owner_cap(cfg, &owner, slots_used, 1, "this spawn")
            .map_err(|e| (1, format!("error: {e} — cannot spawn {}\n", opts.name)))?;
    }

    prepare_worktree(git, &repo, &wt, &opts.name, opts.base.as_deref())?;
    if opts.review {
        if let Err(e) = crate::worker::prompt::readonly::record(&runs_dir, &opts.name, &wt) {
            eprintln!("warning: read-only baseline not recorded ({e}); this review will be void");
        }
    } else {
        let _ = std::fs::remove_file(crate::worker::prompt::readonly::baseline_path(
            &runs_dir, &opts.name,
        ));
    }
    crate::init::ensure_worker_push_guard(&repo).map_err(|e| (1, format!("{e}\n")))?;

    // Worker rules dump
    let rules_dir = crate::workspace::workspace_root().join("rules");
    let rf = runs_dir.join(format!("{}.rules.md", opts.name));
    if let Some(parent) = rf.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut rules_content = String::new();
    let all_rules = rules_dir.join("_all");
    if let Ok(entries) = std::fs::read_dir(all_rules) {
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(c) = std::fs::read_to_string(&p) {
                    rules_content.push_str(&c);
                }
            }
        }
    }
    let repo_name = repo.file_name().and_then(|n| n.to_str()).unwrap_or("repo");
    let repo_rules = rules_dir.join(repo_name);
    if let Ok(entries) = std::fs::read_dir(repo_rules) {
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(c) = std::fs::read_to_string(&p) {
                    rules_content.push_str(&c);
                }
            }
        }
    }
    let _ = std::fs::write(&rf, &rules_content);

    let grid_tab =
        std::env::var("GRID_TAB").unwrap_or_else(|_| default_grid_tab(&owner, repo_name));
    let _ = WorkerMetadata::save_owner(&runs_dir, &opts.name, &owner);
    let _ = WorkerMetadata::save_grid(&runs_dir, &opts.name, &grid_tab);
    let _ = WorkerMetadata::save_repo(&runs_dir, &opts.name, &repo);
    if let Ok(pt) = std::env::var("PIPELINE_TASK") {
        let _ = WorkerMetadata::save_pipeline(&runs_dir, &opts.name, &pt);
    }
    if let Some(base_ref) = &opts.base {
        let _ = crate::state::atomic_write(
            &runs_dir.join(format!("{}.base", opts.name)),
            format!("{base_ref}\n"),
        );
    } else {
        let _ = std::fs::remove_file(runs_dir.join(format!("{}.base", opts.name)));
    }

    let rf_str = rf.to_string_lossy().to_string();
    let runs_str = runs_dir.to_string_lossy().to_string();
    let wt_str = wt.to_string_lossy().to_string();

    // Attempt list: the configured kind, and — only if its agent fails to start —
    // one hop to its fallback kind. The worker name never changes.
    let mut attempts: Vec<Launch> = vec![primary_launch];

    let mut pane_id = String::new();
    let mut idx = 0;
    while idx < attempts.len() {
        let launch = attempts[idx].clone();
        let cmd = build_launch_cmd(
            &launch,
            &opts.name,
            &rf_str,
            &runs_str,
            &wt_str,
            opts.review,
        )?;
        let split_envs = worker_split_envs(&cmd);

        let split_out = herdr
            .pane_split(&wt_str, &owner, &split_envs)
            .map_err(|e| (1, format!("error: herdr pane split failed: {e}\n")))?;
        if !split_out.status.success() {
            return Err((1, String::from_utf8_lossy(&split_out.stderr).to_string()));
        }
        let pane = serde_json::from_str::<Value>(&String::from_utf8_lossy(&split_out.stdout))
            .ok()
            .and_then(|j| {
                j.get("result")
                    .and_then(|r| r.get("pane"))
                    .and_then(|p| p.get("pane_id"))
                    .and_then(|p| p.as_str())
                    .map(String::from)
            })
            .unwrap_or_default();
        if pane.is_empty() {
            return Err((
                1,
                "error: herdr pane split returned no pane_id\n".to_string(),
            ));
        }

        // Report initial metadata tokens. `grid_tab` carries GRID_TAB (the workers
        // tab label, which need not equal the repo-derived group) so the planner,
        // the repair and the pre-prompt check can accept that tab by its exact
        // label instead of guessing from the group token.
        let initial_tokens = [
            "grid_role=worker",
            &format!("grid_group={repo_name}"),
            &format!("grid_owner={owner}"),
            &format!("grid_tab={grid_tab}"),
        ];
        let rep_res = herdr.pane_report_metadata(&pane, &initial_tokens);
        if rep_res.is_err() || !rep_res.unwrap().status.success() {
            let _ = herdr.pane_close(&pane);
            return Err((1, String::new()));
        }

        let arg_refs: Vec<&str> = cmd.agent_args.iter().map(String::as_str).collect();
        let start_res = herdr.agent_start(&opts.name, &launch.harness, &pane, 60000, &arg_refs);
        let mut started = matches!(&start_res, Ok(out) if out.status.success());
        let mut reason = match &start_res {
            Ok(out) if !out.status.success() => start_failure_reason(out),
            Err(_) => "start-failed".to_string(),
            Ok(_) => String::new(),
        };

        if launch.harness == "claude" {
            // Claude Code's first session in a directory tree it has not been
            // trusted in shows a workspace-trust dialog ("Accessing workspace: ...
            // / Quick safety check: is this a project you created or one you
            // trust?") with "No, exit" focused; the worker's brief would be typed
            // into it and lost. The trust decision is per repo and Claude Code
            // writes it down on acceptance, so this costs the wait-output timeout
            // only until the repo is trusted. herdr's own dialog rules do not know
            // this text, so it can report the agent ready — or 'blocked during
            // startup' when the start is slower — with the dialog still up: accept
            // it here the way a person would (one Down to "Yes, I trust this
            // folder", then Enter), then let it clear before continuing.
            if herdr.pane_wait_output(&pane, "trust", 15000) {
                let _ = herdr.pane_send_keys(&pane, "down");
                let _ = herdr.pane_send_keys(&pane, "enter");
                let mut dialog_up = false;
                for _ in 0..8 {
                    dialog_up = claude_trust_dialog_up(herdr, &pane);
                    if !dialog_up {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                if dialog_up {
                    started = false;
                    reason = "trust-dialog".to_string();
                    // No fallback to try: keep the loud, specific failure (the
                    // pane is closed below).
                    if idx != 0 || primary_fallback.is_none() {
                        let _ = herdr.pane_close(&pane);
                        return Err((
                            1,
                            format!(
                                "error: claude workspace-trust dialog did not clear in {pane} after accepting it — nothing was sent to {}; inspect the pane, then retry the spawn\n",
                                opts.name
                            ),
                        ));
                    }
                }
            }
            if !started && !reason.is_empty() && reason != "trust-dialog" {
                // herdr's start timeout is not proof of failure for claude either:
                // herdr only sees 'timed out waiting for agent startup' when the
                // dialog or a slow SessionStart hook held the start back, so wait
                // for idle rather than abandoning a worker that is merely slow.
                let ok = herdr
                    .agent_wait_idle(&opts.name, 120000)
                    .map(|out| out.status.success())
                    .unwrap_or(false);
                if ok {
                    started = true;
                    reason.clear();
                }
            }
        }

        if started {
            let ready_res = herdr.pane_report_metadata(&pane, &["grid_ready=1"]);
            if ready_res.is_err() || !ready_res.unwrap().status.success() {
                let _ = herdr.pane_close(&pane);
                return Err((1, String::new()));
            }
            pane_id = pane;
            break;
        }

        // Agent start failed: close the pane, then (only from the primary
        // attempt, only when a fallback is configured) take one hop to it.
        let _ = herdr.pane_close(&pane);
        if idx == 0 {
            if let Some((fb, fb_kind)) = fallback_kind(cfg, kind_info, &kind)? {
                if opts.review && !fb_kind.can_review {
                    eprintln!(
                        "error: fallback '{fb}' cannot review; refusing fallback for review spawn of '{}'",
                        opts.name
                    );
                    return Err((1, String::new()));
                }
                crate::config::check_spawn_policy(fb, fb_kind, &opts.approve_kinds)?;
                let fb_launch = resolve_launch(fb_kind, fb, cfg)?;
                eprintln!("WARN: fallback: {}→{fb} ({reason})", kind);
                attempts.push(fb_launch);
                idx += 1;
                continue;
            }
        }
        return Err((1, String::new()));
    }

    if pane_id.is_empty() {
        return Err((1, String::new()));
    }

    if opts.review {
        let _ = WorkerMetadata::mark_review(&runs_dir, &opts.name);
    }

    println!(
        "worker={} kind={} pane={} worktree={}",
        opts.name,
        kind,
        pane_id,
        wt.display()
    );
    if let Err((code, msg)) = run_grid_hook(&grid_tab, Some(&owner)) {
        return Err((
            code,
            format!(
                "{msg}worker '{}' is running in pane {}; run `quaqua worker cleanup {}` or `quaqua grid reconcile --tab {}`\n",
                opts.name, pane_id, opts.name, grid_tab
            ),
        ));
    }

    Ok((caller.to_string(), owner))
}

#[cfg(test)]
mod credential_env_tests {
    use super::*;

    #[test]
    fn worker_pane_env_blanks_credentials() {
        let cmd = LaunchCmd {
            split_envs: vec![("OPENCODE_CONFIG_CONTENT".to_string(), "{}".to_string())],
            agent_args: Vec::new(),
        };
        let envs = worker_split_envs(&cmd);
        for secret in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
        ] {
            assert!(
                envs.iter().any(|(k, v)| *k == secret && v.is_empty()),
                "{secret} must be blanked in the worker pane env"
            );
        }
        assert!(envs
            .iter()
            .any(|(k, v)| *k == "OPENCODE_CONFIG_CONTENT" && !v.is_empty()));
    }
}
