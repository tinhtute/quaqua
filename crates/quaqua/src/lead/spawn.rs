//! Project Lead spawn logic (`quaqua lead spawn <slug>`).

use crate::git::GitOps;
use crate::herdr::HerdrOps;
use serde_json::Value;
use std::path::{Path, PathBuf};

use std::time::Duration;

pub const USAGE: &str = "\
# Usage: quaqua lead spawn <slug> [--task \"<kickoff>\"] [--approve-kind <kind>]
#   Start the project Lead for <slug> (AGENTS.md '## Roles (SLP)'): an agent
#   session named <slug>-lead running in the quaqua checkout this script
#   belongs to (normally ~/quaqua). The Lead is not a worker — no worktree,
#   no slot in the 4-worker cap, no grid hook: each SLP team gets its own
#   herdr workspace labelled <slug> (the Lead in its first tab, its Peers grid
#   beside it or in <repo>-workers there); the CoS stays the one Supervisor in
#   its own workspace and reaches the Lead by agent name. quaqua lead cleanup <slug>
#   closes the workspace when the project is done. rules/_all/*.md are the
#   appended system prompt (the spawn-worker convention); the first prompt
#   is a short lead brief pointing at AGENTS.md, projects/<slug>.md and
#   quaqua pipeline. The project integration branch quaqua/<slug>-integ
#   is created from the project repo's default branch when missing —
#   origin/HEAD if set, else main, else master (Peers branch from it --base
#   and are merged back into it at acceptance). Refuses when
#   <slug> is unknown (no row in projects/INDEX.md or no projects/<slug>.md
#   card), when its Lead is already registered with herdr, or when
#   <slug>-lead would exceed herdr's 32-char agent-name limit; on success
#   the row's owner column becomes the Lead name (serialized with
#   quaqua lead cleanup via projects/INDEX.md.lock.d, published by atomic rename).
#   Env: LEAD_KIND (overrides strategy.lead_kind in team.toml).
#   --approve-kind <kind>  approve a lead kind whose team.toml spawn = \"approve\"
#   --task \"<kickoff>\"  after the lead brief lands and the Lead is ready, send
#                         <kickoff> to it (same verified submit as `lead prompt`).
#                         Without --task the command prints
#                         `NEXT: herdr agent prompt <slug>-lead \"<kickoff>\"` on stderr —
#                         a Lead left at 'ready' never starts work by itself.";

use crate::workspace::workspace_root_checked;

/// Resolve the lead kind and launch settings from config and optional LEAD_KIND override.
/// On missing or unknown lead_kind, defaults to config strategy default_kind.
pub fn resolve_lead_launch(
    cfg: &crate::config::Config,
    env_override: Option<&str>,
) -> Result<(String, crate::launch::Launch), (i32, String)> {
    let candidate = env_override
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .or_else(|| cfg.strategy.lead_kind.clone())
        .unwrap_or_else(|| cfg.strategy.default_kind.clone());

    let kind_name = if cfg.kinds.contains_key(&candidate) {
        candidate
    } else {
        cfg.strategy.default_kind.clone()
    };

    let kind_info = cfg.kinds.get(&kind_name).ok_or_else(|| {
        (1, format!("error: default lead kind '{kind_name}' not found in team.toml\n"))
    })?;

    let launch = crate::launch::resolve_launch(kind_info, &kind_name, cfg)?;
    Ok((kind_name, launch))
}
pub fn spawn_lead<G: GitOps, H: HerdrOps>(
    args: &[String],
    git: &G,
    herdr: &H,
) -> Result<(), (i32, String)> {
    if args.is_empty() {
        return Err((1, format!("{}\n", USAGE)));
    }
    let mut slug = String::new();
    let mut approve_kinds: Vec<String> = Vec::new();
    let mut task: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--task" {
            if i + 1 >= args.len() {
                return Err((1, format!("{}\n", USAGE)));
            }
            let val = &args[i + 1];
            if val.trim().is_empty() {
                return Err((1, format!("{}\n", USAGE)));
            }
            task = Some(val.clone());
            i += 2;
        } else if let Some(v) = arg.strip_prefix("--task=") {
            if v.trim().is_empty() {
                return Err((1, format!("{}\n", USAGE)));
            }
            task = Some(v.to_string());
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
        } else if let Some(v) = arg.strip_prefix("--approve-kind=") {
            if v.is_empty() || v.starts_with('-') {
                return Err((1, format!("{}\n", USAGE)));
            }
            approve_kinds.push(v.to_string());
            i += 1;
        } else if arg.starts_with('-') || !slug.is_empty() {
            return Err((1, format!("{}\n", USAGE)));
        } else {
            slug = arg.clone();
            i += 1;
        }
    }
    if slug.is_empty() {
        return Err((1, format!("{}\n", USAGE)));
    }
    if slug.starts_with('-') || !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err((1, format!("{}\n", USAGE)));
    }

    let name = format!("{slug}-lead");
    if name.len() > 32 {
        return Err((
            1,
            format!(
                "lead name '{name}' is {} chars — herdr allows at most 32; use a shorter slug\n",
                name.len()
            ),
        ));
    }

    let root = workspace_root_checked()?;
    let index = root.join("projects/INDEX.md");
    let card = root.join("projects").join(format!("{slug}.md"));

    let index_content = std::fs::read_to_string(&index)
        .map_err(|e| (1, format!("cannot read {}: {e}\n", index.display())))?;

    let mut found = false;
    let mut repo_cell = String::new();
    for line in index_content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('|') {
            let parts: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();
            if parts.len() >= 3 {
                let s = parts[1];
                let r = parts[2];
                if s == slug && r != "repo" {
                    found = true;
                    repo_cell = r.to_string();
                    break;
                }
            }
        }
    }

    if !found {
        return Err((1, format!("unknown project slug '{slug}' — no row in {}\n", index.display())));
    }
    if !card.is_file() {
        return Err((1, format!("no project card {}\n", card.display())));
    }

    // one Lead per slug check via herdr agent get
    if let Ok((_, get_json)) = herdr.agent_get(&name) {
        if let Ok(parsed) = serde_json::from_str::<Value>(&get_json) {
            if parsed.get("result").and_then(|r| r.get("agent")).is_some() {
                return Err((1, format!("a Lead for '{slug}' already exists: {name}\n")));
            }
        }
    }

    // Integration branch check
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let repo_path_str = if let Some(stripped) = repo_cell.strip_prefix("~/") {
        format!("{}/{}", home, stripped)
    } else if repo_cell == "~" {
        home.clone()
    } else {
        repo_cell.clone()
    };
    let repo = Path::new(&repo_path_str);
    let integ = format!("quaqua/{slug}-integ");

    let integ_ref = format!("refs/heads/{}", integ);
    let integ_exists = git
        .run_in(repo, &["rev-parse", "--verify", "--quiet", &integ_ref])
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !integ_exists {
        let mut origin_head = String::new();
        let origin_head_exists = git
            .run_in(repo, &["rev-parse", "--verify", "--quiet", "refs/remotes/origin/HEAD"])
            .map(|o| o.status.success())
            .unwrap_or(false);

        if origin_head_exists {
            if let Ok(out) = git.run_in(repo, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]) {
                if out.status.success() {
                    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    origin_head = s.strip_prefix("origin/").unwrap_or(&s).to_string();
                }
            }
        }

        let mut base_branch: Option<String> = None;
        let candidates = [origin_head.as_str(), "main", "master"];
        for cand in candidates {
            if cand.is_empty() {
                continue;
            }
            let local_ref = format!("refs/heads/{cand}");
            if git.run_in(repo, &["rev-parse", "--verify", "--quiet", &local_ref]).map(|o| o.status.success()).unwrap_or(false) {
                base_branch = Some(cand.to_string());
                break;
            }
            if cand == origin_head.as_str() {
                let remote_ref = format!("refs/remotes/origin/{cand}");
                if git.run_in(repo, &["rev-parse", "--verify", "--quiet", &remote_ref]).map(|o| o.status.success()).unwrap_or(false) {
                    base_branch = Some(format!("origin/{cand}"));
                    break;
                }
            }
        }

        let base = match base_branch {
            Some(b) => b,
            None => {
                return Err((
                    1,
                    format!(
                        "cannot create {integ}: {} has no default branch (no origin/HEAD, main, or master)\n",
                        repo.display()
                    ),
                ));
            }
        };

        let branch_res = git.run_in(repo, &["branch", &integ, &base]);
        match branch_res {
            Ok(out) if !out.status.success() => {
                let err = String::from_utf8_lossy(&out.stderr).to_string();
                return Err((1, err));
            }
            Err(e) => {
                return Err((1, format!("git branch {integ} {base} failed: {e}\n")));
            }
            _ => {}
        }
    }

    // Lead rules: rules/_all/*.md -> runs/<name>.rules.md
    let runs_dir = root.join("runs");
    let _ = std::fs::create_dir_all(&runs_dir);
    let rf = runs_dir.join(format!("{name}.rules.md"));

    let all_rules_dir = root.join("rules/_all");
    let mut rules_content = String::new();
    if let Ok(entries) = std::fs::read_dir(&all_rules_dir) {
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
    let _ = std::fs::write(&rf, rules_content);

    // Resolve caller identity and log at spawn
    let caller = crate::identity::resolve_caller(herdr, &runs_dir);
    let caller_owner = if caller.role == crate::identity::Role::Unknown && caller.name == "unknown" {
        // The single default to QUAQUA_OWNER (or "cos" if unset) when caller identity
        // has no pane tokens (parity with resolve_owner in worker/spawn.rs until P3 gate enforcement).
        std::env::var("QUAQUA_OWNER")
            .ok()
            .filter(|o| !o.is_empty())
            .unwrap_or_else(|| "cos".to_string())
    } else {
        caller.name.clone()
    };
    eprintln!("[identity] lead spawn: slug={slug} caller={caller}");

    // Load config and resolve lead launch
    let base_cfg = crate::config::load_team_config()?;
    let cfg = crate::config::resolve(base_cfg.clone(), Some(&slug)).unwrap_or(base_cfg);
    let lead_env = std::env::var("LEAD_KIND").ok();
    let (lead_kind, launch) = resolve_lead_launch(&cfg, lead_env.as_deref())?;
    let lead_kind_info = cfg.kinds.get(&lead_kind).ok_or_else(|| {
        (1, format!("error: default lead kind '{lead_kind}' not found in team.toml\n"))
    })?;
    crate::config::check_spawn_policy(&lead_kind, lead_kind_info, &approve_kinds)?;

    let root_str = root.to_string_lossy().to_string();
    let runs_str = runs_dir.to_string_lossy().to_string();
    let rf_str = rf.to_string_lossy().to_string();
    let cmd = crate::launch::build_launch_cmd(
        &launch,
        &name,
        &rf_str,
        &runs_str,
        &root_str,
        false,
    )?;

    crate::layout::shadow(
        Some(&caller),
        Some(herdr),
        crate::layout::Verb::Spawn,
        "lead",
        Some(&name),
        crate::layout::SpawnOrigin::Cli,
        Some(&caller_owner),
        Some(&slug),
        &runs_dir,
        Some(&cfg),
    );

    // Herdr workspace create
    let owner_env = format!("QUAQUA_OWNER={name}");
    let mut ws_args = vec![
        "workspace",
        "create",
        "--label",
        &slug,
        "--no-focus",
        "--cwd",
        &root_str,
        "--env",
        &owner_env,
    ];
    let split_env_strs: Vec<String> = cmd
        .split_envs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    for env_str in &split_env_strs {
        ws_args.push("--env");
        ws_args.push(env_str);
    }
    let ws_out = herdr.run(&ws_args).map_err(|e| (1, format!("herdr workspace create failed: {e}\n")))?;
    let ws_str = String::from_utf8_lossy(&ws_out.stdout).to_string();
    let ws_json: Value = serde_json::from_str(&ws_str).map_err(|e| (1, format!("invalid workspace JSON: {e}\n")))?;
    let pane = ws_json
        .get("result")
        .and_then(|r| r.get("root_pane"))
        .and_then(|p| p.get("pane_id"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| (1, "no root_pane.pane_id in workspace JSON\n".to_string()))?;
    let wsid = ws_json
        .get("result")
        .and_then(|r| r.get("workspace"))
        .and_then(|w| w.get("workspace_id"))
        .and_then(|w| w.as_str())
        .ok_or_else(|| (1, "no workspace.workspace_id in workspace JSON\n".to_string()))?;

    // Tag Lead pane as orchestrator
    let grid_owner_tok = format!("grid_owner={name}");
    let meta_out = herdr.run(&[
        "pane",
        "report-metadata",
        pane,
        "--source",
        "herdr-quaqua",
        "--token",
        "grid_role=orchestrator",
        "--token",
        &grid_owner_tok,
    ]).map_err(|e| (1, format!("herdr pane report-metadata failed: {e}\n")))?;
    if !meta_out.status.success() {
        return Err((1, String::from_utf8_lossy(&meta_out.stderr).to_string()));
    }

    // Start agent
    let mut start_args = vec![
        "agent",
        "start",
        &name,
        "--kind",
        &launch.harness,
        "--pane",
        pane,
        "--timeout",
        "60000",
    ];
    for arg in &cmd.agent_args {
        start_args.push(arg);
    }
    let start_out = herdr.run(&start_args).map_err(|e| (1, format!("herdr agent start failed: {e}\n")))?;
    if !start_out.status.success() {
        return Err((1, String::from_utf8_lossy(&start_out.stderr).to_string()));
    }

    // Record .owner file only after the Lead agent successfully starts, so failed
    // spawns leave no phantom owner rows for status scanners (issue R1).
    crate::state::WorkerMetadata::save_owner(&runs_dir, &name, &caller_owner)
        .map_err(|e| (1, format!("cannot write owner file for {name}: {e}\n")))?;
    // Update projects/INDEX.md with mkdir lock
    update_project_index_owner(&root, &slug, &name)?;

    // Prompt Lead: the verified submit (`--wait --until working` + input box
    // empty) is the same path `lead prompt` uses, so a brief that never left
    // the box fails here instead of leaving a Lead at 'ready'.
    let prompt_text = format!(
        "You are the Lead for project '{slug}' (QUAQUA_OWNER={name}) — role and authority: AGENTS.md '## Roles (SLP)' (this repo's root doc; read it, plus models.md for the worker roster).\n- Own the project card projects/{slug}.md: read it now, then keep its '## Now' at the current truth after every outcome.\n- Pipeline-size work runs through quaqua pipeline. Peers: quaqua worker spawn with GRID_TAB={slug}-workers, at most 4 at once. Approve each builder's plan before code: a pipeline pauses with exit 5 'plan-ready', '--resume' continues it.\n- You are not a worker: no worktree, not counted toward the 4-worker cap, and you never edit the project repo yourself — Peers build and commit on their branches; acceptance = you merge Peer branches into the project integration branch {integ} (created from the repo's default branch if it was missing — spawn Peers --base {integ} so diffs stay against it). quaqua internals (scripts/, rules/, memory/) are CoS-only.\n- Resolve any '## Challenge' in runs/<worker>.md before acceptance; every redirect (from anh iu, the Auditor, or any report) reaches you through the CoS — record CoS steering in runs/pipeline-<task>.md; escalate to CoS only on goal/cost changes.\nReply 'ready' once the card is read."
    );
    crate::lead::prompt::send_prompt(herdr, &name, pane, &prompt_text)?;

    println!("lead={name} slug={slug} workspace={wsid} pane={pane}");

    // A Lead that only reads its brief idles at 'ready' and starts nothing
    // (2026-10-23 llmhub-lead): the kickoff is a separate, explicit step.
    match kickoff_plan(task.as_deref(), &name) {
        Kickoff::Send(text) => {
            wait_lead_ready(herdr, &name)?;
            crate::lead::prompt::send_prompt(herdr, &name, pane, &text)?;
            println!("kickoff={name}");
        }
        Kickoff::Next(line) => eprintln!("{line}"),
    }
    Ok(())
}

/// What to do with the kickoff after the lead brief has landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kickoff {
    /// Send this text to the Lead once it is ready.
    Send(String),
    /// Print this instruction; the caller sends the kickoff itself.
    Next(String),
}

/// `--task <text>` → send it; no `--task` → print the exact `herdr agent prompt`
/// line the caller must run. A Lead left at 'ready' never starts work by itself.
pub fn kickoff_plan(task: Option<&str>, name: &str) -> Kickoff {
    match task {
        Some(text) => Kickoff::Send(text.to_string()),
        None => Kickoff::Next(format!("NEXT: herdr agent prompt {name} \"<kickoff>\"")),
    }
}

/// Block until the Lead is ready (`idle`) after its brief. `QUAQUA_LEAD_READY_MS`
/// overrides the default window; a Lead still working past it gets a loud error
/// instead of a kickoff injected into a running turn.
fn wait_lead_ready<H: HerdrOps>(herdr: &H, name: &str) -> Result<(), (i32, String)> {
    let ms = std::env::var("QUAQUA_LEAD_READY_MS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(300_000);
    let ready = herdr
        .agent_wait_idle(name, ms)
        .map(|out| out.status.success())
        .unwrap_or(false);
    if ready {
        return Ok(());
    }
    Err((
        1,
        format!(
            "error: lead '{name}' was spawned but did not become ready within {ms} ms — it is still on its brief; send the kickoff yourself: `quaqua lead prompt {name_without_lead} \"<kickoff>\"`\n",
            name_without_lead = name.strip_suffix("-lead").unwrap_or(name)
        ),
    ))
}

/// Update the owner column for a project slug in projects/INDEX.md.
///
/// On standard LF markdown files with well-formed table rows (`| slug | repo | owner | ... |`),
/// modifies only the owner cell for the matching project row (preserving all other
/// rows and column spacing byte-identically), serialized via mkdir spinlock on
/// `projects/INDEX.md.lock.d` and written via atomic rename.
pub fn update_project_index_owner(
    root: &Path,
    slug: &str,
    new_owner: &str,
) -> Result<(), (i32, String)> {
    let index = root.join("projects/INDEX.md");
    if !index.is_file() {
        return Err((1, format!("no projects/INDEX.md in {}\n", root.display())));
    }

    let lock_dir = root.join("projects/INDEX.md.lock.d");
    let mut locked = false;
    for _ in 0..200 {
        if std::fs::create_dir(&lock_dir).is_ok() {
            locked = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !locked {
        return Err((
            1,
            format!(
                "{} is locked (stale {}? remove it manually)\n",
                index.display(),
                lock_dir.display()
            ),
        ));
    }

    let res = (|| -> Result<(), (i32, String)> {
        let content = std::fs::read_to_string(&index)
            .map_err(|e| (1, format!("cannot read {}: {e}\n", index.display())))?;
        let updated = update_index_content(&content, slug, new_owner);
        let tmp_path = root
            .join("projects")
            .join(format!("INDEX.md.tmp.{}", std::process::id()));
        std::fs::write(&tmp_path, updated)
            .map_err(|e| (1, format!("cannot write {}: {e}\n", tmp_path.display())))?;
        std::fs::rename(&tmp_path, &index)
            .map_err(|e| (1, format!("cannot rename {} to {}: {e}\n", tmp_path.display(), index.display())))?;
        Ok(())
    })();

    let _ = std::fs::remove_dir(&lock_dir);
    res
}

/// Pure string transformation updating the owner column of a markdown table.
///
/// On standard LF markdown files with well-formed table rows (`| slug | repo | owner | ... |`),
/// modifies only the matching slug's owner column while preserving all other rows,
/// headers, comments, and column spacing byte-identically.
pub fn update_index_content(content: &str, slug: &str, new_owner: &str) -> String {
    let mut out = String::with_capacity(content.len() + 16);
    for line in content.lines() {
        if line.trim_start().starts_with('|') {
            let mut parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 4 {
                let s = parts[1].trim();
                let r = parts[2].trim();
                if s == slug && r != "repo" {
                    let replacement = format!(" {new_owner} ");
                    parts[3] = &replacement;
                    out.push_str(&parts.join("|"));
                    out.push('\n');
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Output;

    struct DummyGit;
    impl GitOps for DummyGit {
        fn run_in(&self, _dir: &Path, _args: &[&str]) -> std::io::Result<Output> {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn run(&self, _args: &[&str]) -> std::io::Result<Output> {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn rev_parse_commit(&self, _repo: &Path, _base: &str) -> bool {
            true
        }
        fn worktree_list_porcelain(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
            Ok(Vec::new())
        }
        fn worktree_add(
            &self,
            _repo: &Path,
            _branch: &str,
            _wt: &Path,
            _base: Option<&str>,
        ) -> std::io::Result<Output> {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn branch_exists(&self, _repo: &Path, _branch: &str) -> bool {
            true
        }
        fn worktree_add_existing(
            &self,
            _repo: &Path,
            _branch: &str,
            _wt: &Path,
        ) -> std::io::Result<Output> {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn worktree_remove(&self, _repo: &Path, _wt: &Path) -> std::io::Result<Output> {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn remote_list(&self, _repo: &Path) -> std::io::Result<Vec<String>> {
            Ok(Vec::new())
        }
        fn is_core_bare(&self, _repo: &Path) -> bool {
            false
        }
        fn has_core_worktree(&self, _repo: &Path) -> bool {
            false
        }
        fn enable_worktree_config(&self, _repo: &Path) -> std::io::Result<Output> {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn set_worktree_pushurl_blocked(&self, _wt: &Path, _remote: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn is_git_dir(&self, _cwd: &Path) -> bool {
            true
        }
        fn diff_and_status(&self, _cwd: &Path) -> (String, String) {
            (String::new(), String::new())
        }
    }

    struct DummyHerdr {
        calls: RefCell<Vec<String>>,
        started: std::cell::Cell<bool>,
    }
    impl DummyHerdr {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                started: std::cell::Cell::new(false),
            }
        }
    }
    impl HerdrOps for DummyHerdr {
        fn run(&self, args: &[&str]) -> std::io::Result<Output> {
            self.calls.borrow_mut().push(args.join(" "));
            let stdout = if args.first() == Some(&"workspace") && args.get(1) == Some(&"create") {
                r#"{"result":{"workspace":{"workspace_id":"w1"},"root_pane":{"pane_id":"w1:p1"}}}"#
            } else if args.first() == Some(&"agent") && args.get(1) == Some(&"start") {
                self.started.set(true);
                r#"{"result":{}}"#
            } else {
                r#"{"result":{}}"#
            };
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }
        fn agent_list(&self) -> std::io::Result<String> {
            Ok(r#"{"result":{"agents":[]}}"#.to_string())
        }
        fn agent_get(&self, _name: &str) -> std::io::Result<(bool, String)> {
            if self.started.get() {
                Ok((true, r#"{"result":{"agent":{"agent_status":"working","pane_id":"w1:p1"}}}"#.to_string()))
            } else {
                Ok((false, r#"{"error":{"code":"agent_not_found"}}"#.to_string()))
            }
        }
        fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn agent_start(
            &self,
            _n: &str,
            _k: &str,
            _p: &str,
            _t: u32,
            _a: &[&str],
        ) -> std::io::Result<Output> {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn pane_split(&self, _c: &str, _o: &str, _e: &[(&str, &str)]) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn pane_close(&self, _p: &str) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn pane_report_metadata(&self, _p: &str, _t: &[&str]) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool {
            unimplemented!()
        }
        fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn pane_send_keys(&self, _p: &str, _k: &str) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn agent_send_keys(&self, _n: &str, _key: &str) -> std::io::Result<Output> {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        fn agent_wait_idle(&self, _n: &str, _t: u32) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn agent_prompt_nowait(&self, _n: &str, _p: &str) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn agent_prompt_wait_background(
            &self,
            _n: &str,
            _p: &str,
            _t: u32,
            _f: std::fs::File,
        ) -> std::io::Result<std::process::Child> {
            unimplemented!()
        }
    }

    struct CleanupDir(PathBuf);
    impl Drop for CleanupDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_lead_spawn_writes_caller_owner_when_set() {
        let _lock = crate::test_env::lock_env();
        let _test_guard = crate::test_env::TestEnvVarGuard::set("QUAQUA_TEST", "1");
        let _team_guard = crate::test_env::TestEnvVarGuard::remove("QUAQUA_TEAM_TOML");
        let _lead_guard = crate::test_env::TestEnvVarGuard::remove("LEAD_KIND");

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("quaqua-lead-spawn-caller-{}-{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("projects")).unwrap();
        fs::create_dir_all(root.join("rules/_all")).unwrap();
        let _guard = CleanupDir(root.clone());

        let index_content = "| slug | repo | owner | status | one-line goal |\n|------|------|-------|--------|---------------|\n| child | ~/Lab/child | cos | active | goal |\n";
        fs::write(root.join("projects/INDEX.md"), index_content).unwrap();
        fs::write(root.join("projects/child.md"), "# child card\n").unwrap();

        let _root_guard = crate::test_env::TestEnvVarGuard::set("QUAQUA_ROOT", root.to_str().unwrap());
        let _home_guard = crate::test_env::TestEnvVarGuard::set("HOME", root.to_str().unwrap());
        let _owner_guard = crate::test_env::TestEnvVarGuard::set("QUAQUA_OWNER", "parent-lead");
        let _pane_guard = crate::test_env::TestEnvVarGuard::set("HERDR_PANE_ID", "");

        let git = DummyGit;
        let herdr = DummyHerdr::new();

        let res = spawn_lead(&["child".to_string()], &git, &herdr);
        assert!(res.is_ok(), "spawn_lead failed: {:?}", res.err());

        let owner_file = root.join("runs/child-lead.owner");
        assert!(owner_file.is_file(), "runs/child-lead.owner must exist");
        let owner_content = fs::read_to_string(&owner_file).unwrap();
        assert_eq!(owner_content, "parent-lead\n");
    }
}
