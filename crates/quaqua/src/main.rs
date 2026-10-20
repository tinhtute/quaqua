//! `quaqua` — the orchestration CLI.
//! R0 scaffolded the crate; R1 added `config`; R3 adds `worker` and `cap`.

use clap::{Args, Parser, Subcommand};
use quaqua::cap::count_owner_slots;
use quaqua::config;
use quaqua::git::Git;
use quaqua::herdr::Herdr;
use quaqua::worker::cleanup::{cleanup_worker, default_runs_dir};
use quaqua::worker::prompt::{do_check_brief, prompt_worker, USAGE as PROMPT_USAGE};
use quaqua::worker::spawn::{parse_spawn_args, spawn_worker};
use quaqua::worker::stop::{parse_stop_args, stop_worker};
use std::path::{Path, PathBuf};

const CLEANUP_USAGE: &str = "\
# Usage: cleanup-worker.sh <name> <repo-path>
# Stops the agent pane and removes its worktree. Branch quaqua/<name> is kept.";

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
    Gate,
    /// Boot the per-workspace personal state a fresh clone needs.
    Init,
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

fn team_file(explicit: &Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(p.clone());
    }
    if let Ok(p) = std::env::var("QUAQUA_TEAM_TOML") {
        return Ok(PathBuf::from(p));
    }
    Ok(PathBuf::from("team.toml"))
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
        return Err((1, "error: worker subcommand required (spawn, prompt, cleanup, stop)\n".to_string()));
    }
    match args[0].as_str() {
        "spawn" => {
            let opts = parse_spawn_args(&args[1..])?;
            let cfg = config::load_team_config()?;
            spawn_worker(opts, &cfg, git, herdr)?;
            Ok(())
        }
        "prompt" => {
            let p_args = &args[1..];
            if p_args.is_empty() {
                return Err((1, format!("{PROMPT_USAGE}\n")));
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
                        if !val.chars().all(|c| c.is_ascii_digit()) || val.starts_with('0') || val.is_empty() {
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
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
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
            if task.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
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
            if args.len() != 3 || args[1].is_empty() || args[2].is_empty() {
                return Err((1, format!("{CLEANUP_USAGE}\n")));
            }
            let name = &args[1];
            let repo = Path::new(&args[2]);
            cleanup_worker(name, repo, git, herdr)?;
            Ok(())
        }
        "stop" => {
            let name = parse_stop_args(&args[1..])?;
            stop_worker(&name, herdr)?;
            Ok(())
        }
        other => {
            Err((1, format!("error: unknown worker subcommand '{other}'\n")))
        }
    }
}

fn handle_cap(args: &[String], herdr: &Herdr) -> Result<(), (i32, String)> {
    if args.len() > 2 {
        return Err((1, format!("{CAP_USAGE}\n")));
    }
    let owner = args.first().map(String::as_str).unwrap_or("cos");
    let exclude = args.get(1).map(String::as_str);
    let runs_dir = default_runs_dir();
    let count = count_owner_slots(&runs_dir, owner, exclude, herdr);
    println!("{count}");
    Ok(())
}

fn do_grid_plan(file_opt: Option<PathBuf>, path_opt: Option<PathBuf>) -> Result<(), String> {
    let target = file_opt.or(path_opt);
    let content = match target {
        Some(path) => {
            std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?
        }
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
    let def_statuses = team_file(&None)
        .ok()
        .and_then(|p| config::load(&p).ok())
        .map(|c| c.grid.deferred_statuses);
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
    let (default_workers_tab, def_statuses) = if let Ok(tf) = team_file(&team_opt) {
        let cfg = config::load(&tf).ok();
        (
            cfg.as_ref().map(|c| c.grid.workers_tab.clone()).unwrap_or_else(|| "workers".to_string()),
            cfg.map(|c| c.grid.deferred_statuses),
        )
    } else {
        ("workers".to_string(), None)
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
    quaqua::grid::run_reconcile(&mut driver, &opts, &default_workers_tab, def_slices.as_deref())
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
        Some(Command::Taskforce { args }) => match quaqua::taskforce::handle_taskforce(&args, &git, &herdr) {
            Ok(()) => 0,
            Err((rc, err)) => {
                if !err.is_empty() {
                    eprint!("{err}");
                }
                rc
            }
        },
        Some(Command::Merge { args }) => match quaqua::merge::handle_merge(&args, &git) {
            Ok(()) => 0,
            Err((rc, err)) => {
                if !err.is_empty() {
                    eprint!("{err}");
                }
                rc
            }
        },
        Some(Command::Pipeline { args }) => match quaqua::pipeline::handle_pipeline(&args, &git, &herdr) {
            Ok(()) => 0,
            Err((rc, err)) => {
                if !err.is_empty() {
                    eprint!("{err}");
                }
                rc
            }
        },
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
        Some(Command::Gate) => quaqua::gate::run(),
        Some(Command::Init) => match quaqua::init::run() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
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
        Some(Command::Status { args }) => match quaqua::status::handle_status(&args, &Herdr::new()) {
            Ok(code) => code,
            Err(e) => {
                if !e.is_empty() {
                    eprintln!("{e}");
                }
                1
            }
        },
        None => 0,
    };
    std::process::exit(code);
}
