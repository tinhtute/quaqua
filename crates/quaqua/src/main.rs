//! `quaqua` — the orchestration CLI.
//! R0 scaffolded the crate; R1 added `config`; R3 adds `worker` and `cap`.

use clap::{Args, Parser, Subcommand};
use quaqua::cap::count_owner_slots;
use quaqua::config;
use quaqua::git::Git;
use quaqua::herdr::Herdr;
use quaqua::worker::cleanup::{cleanup_worker, default_runs_dir};
use quaqua::worker::prompt::{
    do_check_brief, prompt_worker, USAGE as PROMPT_USAGE, USAGE_HELP as PROMPT_USAGE_HELP,
};
use quaqua::worker::spawn::{parse_spawn_args, spawn_worker};
use quaqua::worker::stop::{parse_stop_args, stop_worker};
use std::path::{Path, PathBuf};

const CLEANUP_USAGE: &str = "\
# Usage: cleanup-worker.sh <name> [<repo-path>]
# Stops the agent pane and removes its worktree. Branch quaqua/<name> is kept.
# <repo-path> defaults to the repo recorded at spawn (runs/<name>.repo).";

const CAP_USAGE: &str = "\
# Usage: worker-cap.sh <owner> [<exclude-worker>]";

#[derive(Parser)]
#[command(name = "quaqua", version, about = "quaqua orchestration CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Read and check the repo team.toml.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Worker operations: spawn, prompt, cleanup, stop.
    Worker {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print the number of active worker slots used by owner.
    Cap {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Agent grid layout planner and reconciler.
    Grid {
        #[command(subcommand)]
        cmd: GridCmd,
    },
    /// Project Lead operations: spawn, cleanup.
    Lead {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Taskforce operations: advisor, dual-lens.
    Taskforce {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Lead merge queue.
    Merge {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Pipeline orchestration: build -> review -> fix.
    Pipeline {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// PreToolUse(Bash) hook: block direct pane/workspace spawn/move calls.
    Gate {
        /// Run as the SessionStart hook: record the rule baseline (issue #72).
        #[arg(long)]
        session_start: bool,
    },
    /// Check the installed quaqua binary (missing, stale, shadowed), or workspace drift.
    Doctor {
        /// Check workspace rules and herdr plugin registration for drift.
        #[arg(long)]
        workspace: bool,
    },
    /// Boot (or update) a workspace from the embedded template.
    Init {
        /// Target workspace (default: the workspace root).
        dir: Option<PathBuf>,
        /// Reconcile managed template files against .quaqua/template.lock.
        #[arg(long)]
        update: bool,
        /// Print the decisions without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Install only the portable-paths pre-commit into an existing git repo
        /// (no workspace sync).
        #[arg(long, value_name = "REPO", conflicts_with_all = ["dir", "update", "dry_run"])]
        git_hooks: Option<PathBuf>,
    },
    /// OpenCode usage audit.
    Usage {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Read-only drift and pipeline audit.
    Auditor {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show workflow and orchestration status overview.
    Status {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Playbook operations: list, show.
    Playbook {
        #[command(subcommand)]
        cmd: PlaybookCmd,
    },
    /// Chief of Staff operations: tag.
    Cos {
        #[command(subcommand)]
        cmd: CosCmd,
    },
    /// Identity operations: heal.
    Identity {
        #[command(subcommand)]
        cmd: IdentityCmd,
    },
    /// Topology layout operations: show, why.
    Layout {
        #[command(subcommand)]
        cmd: LayoutCmd,
    },
    /// Inter-team and worker mail: send, inbox, import, ls.
    Mail {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show trigger and incident digest for a consumer.
    Inbox {
        /// Recipient / consumer to read for (defaults to resolved caller if omitted)
        #[arg(long)]
        r#for: Option<String>,
        /// Do not advance cursor (peek mode)
        #[arg(long)]
        peek: bool,
        /// Continuously follow and print new items every 2 seconds
        #[arg(long)]
        follow: bool,
        /// Show all triggers ignoring high-water mark
        #[arg(long)]
        all: bool,
        /// Output format: plain, hook-ups, or hook-stop
        #[arg(long, default_value = "plain")]
        format: String,
    },
    /// Event bus operations: hook.
    Events {
        #[command(subcommand)]
        cmd: EventsCmd,
    },
    /// Farm bus operations: migrate.
    Bus {
        #[command(subcommand)]
        cmd: BusCmd,
    },
    /// Event source operations: poll.
    Source {
        #[command(subcommand)]
        cmd: SourceCmd,
    },
}

#[derive(Subcommand)]
enum SourceCmd {
    /// GitHub CI source: record a ci_failed trigger per failed run.
    GithubCi {
        #[command(subcommand)]
        cmd: GithubCiCmd,
    },
}

#[derive(Subcommand)]
enum GithubCiCmd {
    /// Poll the engine repo once and record new ci_failed triggers.
    Poll,
}

#[derive(Subcommand)]
enum BusCmd {
    /// Migrate legacy database schema to superagent triggers and threads.
    Migrate {
        /// Dry run: report counts and new cursors without writing changes.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum GridCmd {
    /// Pure layout planner: snapshot JSON on stdin/file -> ops JSON on stdout.
    Plan {
        /// Path to snapshot JSON file (default: stdin).
        #[arg(long, short)]
        file: Option<PathBuf>,
        /// Optional positional snapshot file path.
        path: Option<PathBuf>,
    },
    /// Reconcile worker panes into the agent grid layout.
    Reconcile {
        /// Block until grid check has no pending moves for these workers.
        #[arg(long)]
        check: bool,
        /// Repair / arrange workers into grid (default).
        #[arg(long)]
        repair: bool,
        /// Label of the worker tab (default: from team.toml [grid] or 'workers').
        #[arg(long)]
        tab: Option<String>,
        /// Comma-separated worker names to scope moves.
        #[arg(long)]
        only: Option<String>,
        /// Owner scope (default: $QUAQUA_OWNER or 'cos').
        #[arg(long)]
        owner: Option<String>,
        /// Print planned actions without executing.
        #[arg(long)]
        dry_run: bool,
        /// Path to team.toml.
        #[arg(long)]
        team_file: Option<PathBuf>,
    },
    /// Herdr event hook reconciler.
    Hook,
}
#[derive(Subcommand)]
enum CosCmd {
    /// Tag the CoS pane with metadata and label.
    Tag {
        /// Pane ID to tag (default: current pane).
        #[arg(long)]
        pane: Option<String>,
    },
}
#[derive(Subcommand)]
enum IdentityCmd {
    /// Heal lost pane identity tokens after herdr restart.
    Heal {
        /// Optional pane ID to heal (default: all panes).
        #[arg(long)]
        pane: Option<String>,
        /// Preview actions without modifying panes or files.
        #[arg(long)]
        dry_run: bool,
    },
}
#[derive(Subcommand)]
enum LayoutCmd {
    /// Show effective topology layout.
    Show {
        /// Optional project slug
        slug: Option<String>,
        /// Project slug override
        #[arg(long)]
        project: Option<String>,
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Explain layout decision for a caller, verb, and target.
    Why {
        caller: String,
        verb: String,
        target: String,
        /// Project slug override
        #[arg(long)]
        project: Option<String>,
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Refreeze project layout snapshot from live team.toml.
    Refreeze {
        /// Project slug
        slug: String,
    },
}
#[derive(Subcommand)]
enum EventsCmd {
    /// Herdr event hook for monitoring agent status changes and exits.
    Hook,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Validate team.toml (exit 0 = ok, non-zero = rejected with a clear error).
    Validate(ConfigArgs),
    /// Print the resolved value of a dotted key, e.g. strategy.default_reviewer.
    Get {
        /// Dotted key, e.g. `strategy.default_reviewer`.
        key: String,
        #[command(flatten)]
        args: ConfigArgs,
    },
    /// Migrate team.toml from schema_version 2 to 3.
    Migrate {
        /// Write changes in-place (keeps team.toml.bak). Default is dry-run diff.
        #[arg(long)]
        write: bool,
        /// Path to team.toml (default: resolved team.toml).
        #[arg(long)]
        path: Option<PathBuf>,
    },
}
#[derive(Args)]
struct ConfigArgs {
    /// Path to team.toml (default: $QUAQUA_TEAM_TOML, else ./team.toml, else
    /// <workspace>/team.toml, else the embedded default).
    #[arg(long)]
    file: Option<PathBuf>,
    /// Validate the config as seen by this project (applies its override).
    #[arg(long)]
    project: Option<String>,
}

#[derive(Subcommand)]
enum PlaybookCmd {
    /// List available playbooks (<name> — <when>).
    List {
        /// Optional path to playbooks directory.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Optional path to principles directory.
        #[arg(long)]
        principles_dir: Option<PathBuf>,
    },
    /// Show a playbook's Orchestrator section.
    Show {
        /// Playbook name.
        name: String,
        /// Optional path to playbooks directory.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Optional path to principles directory.
        #[arg(long)]
        principles_dir: Option<PathBuf>,
    },
}

/// Grid config source: the explicit `--team-file`, else the same resolution
/// every other command uses (`config::load_team_config`). `None` when no file
/// resolves (or it cannot be parsed), so the grid falls back to its built-in
/// labels and statuses.
fn grid_config(explicit: &Option<PathBuf>) -> Option<config::Config> {
    match explicit {
        Some(p) => config::load(p).ok(),
        None => config::load_team_config().ok(),
    }
}

/// `config get|validate` config source: the explicit `--file`, else the same
/// resolution every other command uses (`config::load_team_config`).
fn load_config(explicit: &Option<PathBuf>) -> Result<config::Config, String> {
    if let Some(p) = explicit {
        return config::load(p);
    }
    // load_team_config pre-formats its message for callers that `eprint!` it
    // verbatim; the config command prints its own `error: ` prefix.
    config::load_team_config().map_err(|(_, msg)| {
        msg.trim()
            .strip_prefix("error: ")
            .unwrap_or(msg.trim())
            .to_string()
    })
}

fn do_validate(args: &ConfigArgs) -> Result<(), String> {
    let cfg = load_config(&args.file)?;
    // Validate the raw file once. `validate` already bounds every [projects.*]
    // override (and the weights) against the top-level cap, so a bad override is
    // caught even without --project.
    config::validate(&cfg)?;
    // Selecting a project only needs it to exist: its override was already
    // checked against the global baseline above. Re-validating a resolved config
    // would wrongly re-check the *other* projects against this project's cap.
    if let Some(slug) = &args.project {
        if !cfg.projects.contains_key(slug) {
            return Err(format!("unknown project '{slug}'"));
        }
    }
    Ok(())
}

fn do_get(key: &str, args: &ConfigArgs) -> Result<String, String> {
    let cfg = load_config(&args.file)?;
    config::get(&cfg, key, args.project.as_deref())
}

fn handle_worker(args: &[String], git: &Git, herdr: &Herdr) -> Result<(), (i32, String)> {
    if args.is_empty() {
        return Err((
            1,
            "error: worker subcommand required (spawn, prompt, cleanup, stop)\n".to_string(),
        ));
    }
    match args[0].as_str() {
        "spawn" => {
            let opts = parse_spawn_args(&args[1..])?;
            let cfg = config::load_team_config()?;
            spawn_worker(opts, &cfg, git, herdr, quaqua::layout::SpawnOrigin::Cli)?;
            Ok(())
        }
        "prompt" => {
            let p_args = &args[1..];
            if p_args.is_empty() {
                return Err((1, format!("{PROMPT_USAGE}\n")));
            }
            if p_args[0] == "--help" || p_args[0] == "-h" {
                println!("{PROMPT_USAGE}");
                println!("{PROMPT_USAGE_HELP}");
                return Ok(());
            }
            if p_args[0] == "--check-brief" {
                if p_args.len() != 2 || p_args[1].is_empty() {
                    return Err((1, format!("{PROMPT_USAGE}\n")));
                }
                do_check_brief(&p_args[1])?;
                return Ok(());
            }
            if p_args[0] == "--resume" {
                let mut name = "";
                let mut extend: Option<u64> = None;
                let mut i = 1;
                while i < p_args.len() {
                    let a = &p_args[i];
                    if a == "--extend" {
                        if i + 1 >= p_args.len() {
                            return Err((1, format!("{PROMPT_USAGE}\n")));
                        }
                        let val = &p_args[i + 1];
                        if !val.chars().all(|c| c.is_ascii_digit())
                            || val.starts_with('0')
                            || val.is_empty()
                        {
                            return Err((1, format!("{PROMPT_USAGE}\n")));
                        }
                        extend = val.parse::<u64>().ok();
                        i += 2;
                    } else if a.starts_with('-') {
                        return Err((1, format!("{PROMPT_USAGE}\n")));
                    } else {
                        if !name.is_empty() {
                            return Err((1, format!("{PROMPT_USAGE}\n")));
                        }
                        name = a.as_str();
                        i += 1;
                    }
                }
                if name.is_empty()
                    || !name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                {
                    return Err((1, format!("{PROMPT_USAGE}\n")));
                }
                prompt_worker(name, None, true, extend, git, herdr)?;
                return Ok(());
            }

            // Normal prompt: <name> <task>
            if p_args.len() != 2 {
                return Err((1, format!("{PROMPT_USAGE}\n")));
            }
            let name = &p_args[0];
            let task = &p_args[1];
            if task.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return Err((1, format!("{PROMPT_USAGE}\n")));
            }
            if !quaqua::worker::prompt::brief_headers_ok(task) {
                return Err((
                    1,
                    "error: task must be a brief with 'Goal:', 'Hard constraints:', 'Current design (open to challenge):' (rules/_all/brief.md)\n".to_string(),
                ));
            }
            prompt_worker(name, Some(task), false, None, git, herdr)?;
            Ok(())
        }
        "cleanup" => {
            if !(2..=3).contains(&args.len()) || args[1..].iter().any(String::is_empty) {
                return Err((1, format!("{CLEANUP_USAGE}\n")));
            }
            let name = &args[1];
            let explicit = args.get(2).map(Path::new);
            let Some(repo) =
                quaqua::worker::cleanup::resolve_cleanup_repo(name, explicit, &default_runs_dir())
                    .map_err(|e| (1, e))?
            else {
                return Err((1, format!("{CLEANUP_USAGE}\n")));
            };
            cleanup_worker(name, &repo, git, herdr)?;
            Ok(())
        }
        "stop" => {
            let name = parse_stop_args(&args[1..])?;
            stop_worker(&name, herdr)?;
            Ok(())
        }
        other => Err((1, format!("error: unknown worker subcommand '{other}'\n"))),
    }
}

fn handle_cap(args: &[String], herdr: &Herdr) -> Result<(), (i32, String)> {
    if args.len() > 2 {
        return Err((1, format!("{CAP_USAGE}\n")));
    }
    let owner = args.first().map(String::as_str).unwrap_or("cos");
    let exclude = args.get(1).map(String::as_str);
    let runs_dir = default_runs_dir();
    let cfg = config::load_team_config()?;
    let count = count_owner_slots(&runs_dir, owner, exclude, herdr, &cfg);
    println!("{count}");
    Ok(())
}

fn do_grid_plan(file_opt: Option<PathBuf>, path_opt: Option<PathBuf>) -> Result<(), String> {
    let target = file_opt.or(path_opt);
    let content = match target {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?,
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("cannot read stdin: {e}"))?;
            buf
        }
    };

    let val: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("invalid snapshot JSON: {e}"))?;
    let snap = quaqua::grid::parse_snapshot_value(&val)?;
    let def_statuses = grid_config(&None).map(|c| c.grid.deferred_statuses);
    let def_slices: Option<Vec<&str>> = def_statuses
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect());
    let ops = quaqua::grid::plan_with_deferred(&snap, def_slices.as_deref())?;
    let json_out = serde_json::to_string(&ops).map_err(|e| e.to_string())?;
    println!("{json_out}");
    Ok(())
}

fn do_grid_reconcile(
    check: bool,
    repair: bool,
    tab: Option<String>,
    only: Option<String>,
    owner: Option<String>,
    dry_run: bool,
    team_opt: Option<PathBuf>,
) -> Result<(), String> {
    let cfg = grid_config(&team_opt);
    let (default_workers_tab, def_statuses) = match cfg {
        Some(c) => (c.grid.workers_tab, Some(c.grid.deferred_statuses)),
        None => ("workers".to_string(), None),
    };

    if check && repair {
        return Err("cannot specify both --check and --repair".to_string());
    }
    if tab.as_deref() == Some("") {
        return Err("--tab requires a non-empty label".to_string());
    }

    let opts = quaqua::grid::ReconcileOptions {
        check,
        repair: if check { false } else { true },
        tab,
        only,
        owner,
        dry_run,
        timeout_sec: None,
    };
    let mut driver = quaqua::grid::CliHerdrDriver::new();
    let def_slices: Option<Vec<&str>> = def_statuses
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect());
    quaqua::grid::run_reconcile(
        &mut driver,
        &opts,
        &default_workers_tab,
        def_slices.as_deref(),
    )
}
fn main() {
    let cli = Cli::parse();
    let git = Git::new();
    let herdr = Herdr::new();

    let code = match cli.command {
        Some(Command::Config { cmd }) => match cmd {
            ConfigCmd::Validate(args) => match do_validate(&args) {
                Ok(()) => {
                    println!("ok");
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            },
            ConfigCmd::Get { key, args } => match do_get(&key, &args) {
                Ok(value) => {
                    println!("{value}");
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            },
            ConfigCmd::Migrate { write, path } => {
                match quaqua::layout::migrate::run_migrate(path.as_deref(), write) {
                    Ok(out) => {
                        print!("{out}");
                        0
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        1
                    }
                }
            }
        },
        Some(Command::Cos { cmd }) => match cmd {
            CosCmd::Tag { pane } => match quaqua::cos::resolve_cos_pane(&herdr, pane.as_deref()) {
                Ok(p) => match quaqua::cos::tag_cos_pane(&herdr, &p) {
                    Ok(msg) => {
                        print!("{msg}");
                        0
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        1
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            },
        },
        Some(Command::Identity { cmd }) => match cmd {
            IdentityCmd::Heal { pane, dry_run } => {
                let runs_dir = default_runs_dir();
                match quaqua::identity::heal_with_runs_dir(
                    &herdr,
                    &runs_dir,
                    pane.as_deref(),
                    dry_run,
                ) {
                    Ok(report) => {
                        for action in &report.actions {
                            println!("{action}");
                        }
                        for err in &report.errors {
                            eprintln!("error: {err}");
                        }
                        if dry_run && report.actions.is_empty() {
                            println!("ok: no panes need healing");
                        }
                        if report.actions.is_empty() && !report.errors.is_empty() {
                            1
                        } else {
                            0
                        }
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        1
                    }
                }
            }
        },
        Some(Command::Worker { args }) => match handle_worker(&args, &git, &herdr) {
            Ok(()) => 0,
            Err((rc, err)) => {
                if !err.is_empty() {
                    eprint!("{err}");
                }
                rc
            }
        },
        Some(Command::Cap { args }) => match handle_cap(&args, &herdr) {
            Ok(()) => 0,
            Err((rc, err)) => {
                if !err.is_empty() {
                    eprint!("{err}");
                }
                rc
            }
        },
        Some(Command::Lead { args }) => match quaqua::lead::handle_lead(&args, &git, &herdr) {
            Ok(()) => 0,
            Err((rc, err)) => {
                if !err.is_empty() {
                    eprint!("{err}");
                }
                rc
            }
        },
        Some(Command::Taskforce { args }) => {
            match quaqua::taskforce::handle_taskforce(&args, &git, &herdr) {
                Ok(()) => 0,
                Err((rc, err)) => {
                    if !err.is_empty() {
                        eprint!("{err}");
                    }
                    rc
                }
            }
        }
        Some(Command::Merge { args }) => match quaqua::merge::handle_merge(&args, &git) {
            Ok(()) => 0,
            Err((rc, err)) => {
                if !err.is_empty() {
                    eprint!("{err}");
                }
                rc
            }
        },
        Some(Command::Pipeline { args }) => {
            match quaqua::pipeline::handle_pipeline(&args, &git, &herdr) {
                Ok(()) => 0,
                Err((rc, err)) => {
                    if !err.is_empty() {
                        eprint!("{err}");
                    }
                    rc
                }
            }
        }
        Some(Command::Grid { cmd }) => match cmd {
            GridCmd::Plan { file, path } => match do_grid_plan(file, path) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            },
            GridCmd::Reconcile {
                check,
                repair,
                tab,
                only,
                owner,
                dry_run,
                team_file,
            } => match do_grid_reconcile(check, repair, tab, only, owner, dry_run, team_file) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            },
            GridCmd::Hook => match quaqua::grid::run_hook() {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            },
        },
        Some(Command::Gate {
            session_start: false,
        }) => quaqua::gate::run(),
        Some(Command::Gate {
            session_start: true,
        }) => quaqua::gate::run_session_start(),
        Some(Command::Doctor { workspace }) => {
            if workspace {
                quaqua::doctor::run_workspace(None, None, None)
            } else {
                quaqua::doctor::run()
            }
        }
        Some(Command::Init {
            dir,
            update,
            dry_run,
            git_hooks,
        }) => match git_hooks {
            Some(repo) => match quaqua::init::run_git_hooks(&repo) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            },
            None => match quaqua::init::run(dir.as_deref(), update, dry_run) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            },
        },
        Some(Command::Usage { args }) => match quaqua::usage::handle_usage(&args) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        Some(Command::Auditor { args }) => match quaqua::auditor::handle_auditor(&args) {
            Ok(code) => code,
            Err((code, msg)) => {
                if !msg.is_empty() {
                    eprint!("{msg}");
                }
                code
            }
        },
        Some(Command::Status { args }) => match quaqua::status::handle_status(&args, &Herdr::new())
        {
            Ok(code) => code,
            Err(e) => {
                if !e.is_empty() {
                    eprintln!("{e}");
                }
                1
            }
        },
        Some(Command::Playbook { cmd }) => match cmd {
            PlaybookCmd::List {
                dir,
                principles_dir,
            } => {
                match (
                    quaqua::playbook::resolve_playbooks_dir(dir.as_deref()),
                    quaqua::playbook::resolve_principles_dir(principles_dir.as_deref()),
                ) {
                    (Ok(pb_dir), Ok(pr_dir)) => {
                        match quaqua::playbook::run_list(&pb_dir, &pr_dir) {
                            Ok(out) => {
                                print!("{out}");
                                0
                            }
                            Err(e) => {
                                eprintln!("error: {e}");
                                1
                            }
                        }
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        eprintln!("error: {e}");
                        1
                    }
                }
            }
            PlaybookCmd::Show {
                name,
                dir,
                principles_dir,
            } => {
                match (
                    quaqua::playbook::resolve_playbooks_dir(dir.as_deref()),
                    quaqua::playbook::resolve_principles_dir(principles_dir.as_deref()),
                ) {
                    (Ok(pb_dir), Ok(pr_dir)) => {
                        match quaqua::playbook::run_show(&name, &pb_dir, &pr_dir) {
                            Ok(out) => {
                                print!("{out}");
                                0
                            }
                            Err(e) => {
                                eprintln!("error: {e}");
                                1
                            }
                        }
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        eprintln!("error: {e}");
                        1
                    }
                }
            }
        },
        Some(Command::Layout { cmd }) => match cmd {
            LayoutCmd::Show {
                slug,
                project,
                json,
            } => {
                let proj = slug.as_deref().or(project.as_deref());
                match quaqua::layout::run_show(proj, json) {
                    Ok(out) => {
                        print!("{out}");
                        0
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        1
                    }
                }
            }
            LayoutCmd::Why {
                caller,
                verb,
                target,
                project,
                json,
            } => match quaqua::layout::run_why(&caller, &verb, &target, project.as_deref(), json) {
                Ok(out) => {
                    print!("{out}");
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            },
            LayoutCmd::Refreeze { slug } => match quaqua::layout::run_refreeze(&slug) {
                Ok(msg) => {
                    println!("{msg}");
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            },
        },
        Some(Command::Mail { args }) => {
            match quaqua::bus::mail::handle_mail(&args, &Herdr::new()) {
                Ok(code) => code,
                Err(e) => {
                    if !e.is_empty() {
                        eprintln!("{e}");
                    }
                    if e.contains(quaqua::bus::LEGACY_DB_ERROR_MSG) {
                        2
                    } else {
                        1
                    }
                }
            }
        }
        Some(Command::Inbox {
            r#for,
            peek,
            follow,
            all,
            format,
        }) => {
            let runs_dir = if let Ok(dir) = std::env::var("QUAQUA_RUNS_DIR") {
                PathBuf::from(dir)
            } else {
                quaqua::workspace::workspace_root().join("runs")
            };
            let db_path = runs_dir.join("quaqua.db");
            match quaqua::bus::FarmBus::open(&db_path) {
                Ok(bus) => {
                    let herdr = quaqua::herdr::Herdr::new();
                    let caller = quaqua::identity::resolve_caller(&herdr, &runs_dir);

                    let (consumer, should_advance) = match r#for {
                        None => {
                            if caller.role == quaqua::identity::Role::Unknown {
                                eprintln!(
                                    "cannot determine recipient identity from harness (unresolved caller)"
                                );
                                std::process::exit(1);
                            }
                            if caller.role == quaqua::identity::Role::Peer {
                                eprintln!(
                                    "warning: peer caller cannot advance lead/cos cursor; rendering in peek mode without advancing cursor"
                                );
                                (caller.name, false)
                            } else {
                                (caller.name, !peek && !all)
                            }
                        }
                        Some(target) => {
                            if caller.role == quaqua::identity::Role::Unknown {
                                eprintln!(
                                    "warning: cannot determine caller identity from harness (unresolved caller); rendering in peek mode without advancing cursor"
                                );
                                (target, false)
                            } else if caller.role == quaqua::identity::Role::Peer
                                && (target == "cos" || quaqua::cap::is_lead_owner(&target))
                            {
                                eprintln!(
                                    "warning: peer caller '{}' cannot advance cursor for '{}'; rendering in peek mode without advancing cursor",
                                    caller.name, target
                                );
                                (target, false)
                            } else if caller.name != target {
                                eprintln!(
                                    "warning: caller '{}' does not match recipient '{}'; rendering in peek mode without advancing cursor",
                                    caller.name, target
                                );
                                (target, false)
                            } else {
                                (target, !peek && !all)
                            }
                        }
                    };

                    if !quaqua::bus::mail::is_safe_inbox_name(&consumer) {
                        eprintln!("invalid recipient name '{consumer}': cannot escape runs/");
                        std::process::exit(1);
                    }
                    let deliver_format = match quaqua::bus::events::DeliverFormat::parse(&format) {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("error: {e}");
                            std::process::exit(1);
                        }
                    };
                    let mode = quaqua::bus::events::DeliverMode::new(!should_advance, all);

                    if follow {
                        let mut stdout = quaqua::bus::events::SafeStdout;
                        let mut in_memory_cursor: Option<u64> = None;
                        loop {
                            let iter_mode = match (mode, in_memory_cursor) {
                                (m, Some(cur)) if !m.should_advance() => {
                                    quaqua::bus::events::DeliverMode::PeekFrom(cur)
                                }
                                (m, _) => m,
                            };
                            match bus.deliver_page(
                                &consumer,
                                iter_mode,
                                deliver_format,
                                &mut stdout,
                            ) {
                                Ok(outcome) => {
                                    if !mode.should_advance() {
                                        in_memory_cursor = Some(outcome.new_cursor);
                                    }
                                }
                                Err(e) => {
                                    if e.contains("Broken pipe") || e.contains("os error 32") {
                                        std::process::exit(0);
                                    }
                                    eprintln!("inbox error: {e}");
                                }
                            }
                            std::thread::sleep(std::time::Duration::from_secs(2));
                        }
                    } else {
                        let mut stdout = quaqua::bus::events::SafeStdout;
                        match bus.deliver_page(&consumer, mode, deliver_format, &mut stdout) {
                            Ok(_) => 0,
                            Err(e) => {
                                eprintln!("error: {e}");
                                1
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error opening bus at {}: {e}", db_path.display());
                    if quaqua::bus::is_legacy_db_error(&e) {
                        2
                    } else {
                        1
                    }
                }
            }
        }
        Some(Command::Events { cmd }) => match cmd {
            EventsCmd::Hook => match quaqua::bus::events::run_events_hook() {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            },
        },
        Some(Command::Bus { cmd }) => match cmd {
            BusCmd::Migrate { dry_run } => match quaqua::bus::migrate::run_migrate(dry_run) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            },
        },
        Some(Command::Source { cmd }) => match cmd {
            SourceCmd::GithubCi { cmd } => match cmd {
                GithubCiCmd::Poll => match quaqua::source::github_ci::run_poll(&default_runs_dir())
                {
                    Ok(code) => code,
                    Err(e) => {
                        eprintln!("error: {e}");
                        1
                    }
                },
            },
        },
        None => 0,
    };
    std::process::exit(code);
}
