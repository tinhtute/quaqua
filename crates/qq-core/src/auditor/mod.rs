//! `quaqua auditor run`: the read-only drift and pipeline audit.
//!
//! Runs the deterministic pre-checks over the workspace, hands them to a
//! headless model run, and writes the answer to `reports/audit-YYYY-MM-DD-HH.md`.
//! The only file written is that report. The model comes from `team.toml`
//! (the `review` purpose's kind) through the driver registry.

mod ack;
mod clock;
mod headless;
mod precheck;
mod prompt;

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::{CliError, Command as CliCommand, Env, Install, Registry};
use crate::daemon::Daemon;
use crate::driver::{write_atomic, Registry as Drivers};

pub use precheck::{run_deterministic_prechecks, run_deterministic_prechecks_at};
pub use prompt::PROMPT_TEMPLATE;

pub const USAGE: &str = "usage: quaqua auditor run [--dry-run] [--out <path>] [--model <model>] [--kind <kind>] [--repo <path>]";

/// The purpose whose kind audits (config `[purposes]`).
const PURPOSE: &str = "review";

pub const INSTALL: Install = install;

fn install(registry: &mut Registry, _daemon: &mut Daemon) -> Result<(), String> {
    registry.register(CliCommand {
        name: "auditor",
        help: "auditor run: read-only drift audit to reports/audit-<date>-<HH>.md",
        run: auditor_cmd,
    })
}

#[derive(Debug, Default, Clone)]
pub struct AuditorRunOptions {
    pub dry_run: bool,
    pub out: Option<PathBuf>,
    /// Overrides the model of the resolved kind.
    pub model: Option<String>,
    /// Overrides the kind (default: the `review` purpose's kind).
    pub kind: Option<String>,
    pub repo: Option<PathBuf>,
    /// Runs this program in place of the harness binary (tests). Not a CLI flag.
    pub exec: Option<PathBuf>,
}

fn usage_err(msg: &str) -> CliError {
    CliError::new(2, format!("{msg}\n{USAGE}"))
}

pub fn parse_args(args: &[String]) -> Result<Option<AuditorRunOptions>, CliError> {
    match args.first().map(String::as_str) {
        Some("run") => {}
        Some("--help" | "-h") => return Ok(None),
        Some(other) => return Err(usage_err(&format!("unknown auditor command '{other}'"))),
        None => return Err(usage_err("missing command")),
    }
    let mut o = AuditorRunOptions::default();
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (arg, None),
        };
        if matches!(name, "--help" | "-h") {
            return Ok(None);
        }
        if name == "--dry-run" {
            o.dry_run = true;
            i += 1;
            continue;
        }
        if !matches!(name, "--out" | "--model" | "--kind" | "--repo") {
            return Err(usage_err(&format!("unknown auditor option '{arg}'")));
        }
        let (val, step) = match inline {
            Some(v) => (v, 1),
            None => match args.get(i + 1) {
                Some(v) => (v.clone(), 2),
                None => return Err(usage_err(&format!("{name} requires a value"))),
            },
        };
        match name {
            "--out" => o.out = Some(PathBuf::from(val)),
            "--model" => o.model = Some(val),
            "--kind" => o.kind = Some(val),
            _ => o.repo = Some(PathBuf::from(val)),
        }
        i += step;
    }
    Ok(Some(o))
}

fn auditor_cmd(_env: &Env, args: &[String]) -> Result<String, CliError> {
    match parse_args(args)? {
        None => Ok(USAGE.to_string()),
        Some(opts) => run_auditor(opts).map(|p| p.display().to_string()),
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn resolve_root(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| env_nonempty("AUDITOR_REPO").map(PathBuf::from))
        .unwrap_or_else(crate::db::path::workspace_root)
}

fn resolve_out(root: &Path, explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| env_nonempty("AUDITOR_OUT").map(PathBuf::from))
        .unwrap_or_else(|| {
            root.join("reports")
                .join(format!("audit-{}.md", clock::date_hour()))
        })
}

/// Harness, model and effort the audit runs with, read from `<root>/team.toml`.
struct Target {
    harness: String,
    model: String,
    effort: Option<String>,
}

fn resolve_target(root: &Path, opts: &AuditorRunOptions) -> Result<Target, String> {
    let path = root.join("team.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let doc: toml::Table = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    let kind = match &opts.kind {
        Some(k) => k.clone(),
        None => doc
            .get("purposes")
            .and_then(|p| p.get(PURPOSE))
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{} has no purposes.{PURPOSE}", path.display()))?
            .to_string(),
    };
    let row = doc
        .get("kinds")
        .and_then(|k| k.get(&kind))
        .ok_or_else(|| format!("{} has no kinds.{kind}", path.display()))?;
    let field = |k: &str| row.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let harness = field("harness").ok_or_else(|| format!("kinds.{kind} has no harness"))?;
    let model = opts
        .model
        .clone()
        .or_else(|| env_nonempty("AUDITOR_MODEL"))
        .or_else(|| field("model"))
        .ok_or_else(|| format!("kinds.{kind} has no model"))?;
    Ok(Target {
        harness,
        model,
        effort: field("effort"),
    })
}

fn dry_run_report(flags: &[String]) -> String {
    let pick = |keep: &dyn Fn(&str) -> bool| -> String {
        let lines: Vec<&str> = flags
            .iter()
            .map(String::as_str)
            .filter(|f| keep(f))
            .collect();
        if lines.is_empty() {
            "- none\n\n".to_string()
        } else {
            format!("{}\n\n", lines.join("\n"))
        }
    };
    let mut r = String::from("## Drift\n- none\n\n## Stale docs\n");
    r.push_str(&pick(&|f| {
        f.contains("doc-rot risk") || f.contains("expired rule") || f.contains("malformed expiry")
    }));
    r.push_str("## Unhandled\n");
    r.push_str(&pick(&|f| {
        f.contains("unpromoted") || f.contains("recurring failure")
    }));
    r.push_str("## Pipeline\n");
    r.push_str(&pick(&|f| {
        f.starts_with("- runs/pipeline-")
            || (f.starts_with("- runs/") && f.contains("'## Challenge'"))
            || (f.starts_with("- mail #") && !f.contains("live worker"))
    }));
    r.push_str("## Workers\n");
    r.push_str(&pick(&|f| {
        f.contains("herdr") || f.contains("live worker") || f.contains("lost momentum")
    }));
    if flags.is_empty() {
        r.push_str("Verdict: clean\n");
    } else {
        r.push_str(&format!("Verdict: {} findings\n", flags.len()));
    }
    r
}

/// Writes the FAILED stand-in so the CoS sees the miss, then returns the error.
fn fail(out: &Path, code: i32, what: &str) -> CliError {
    let body = format!(
        "# Auditor {} — FAILED\n\n{what}; see scheduler stderr logs (journal).\n",
        clock::date_minute()
    );
    let _ = write_atomic(out, body.as_bytes());
    CliError::new(code, what.to_string())
}

/// Runs one audit; returns the report path.
pub fn run_auditor(opts: AuditorRunOptions) -> Result<PathBuf, CliError> {
    let root = resolve_root(opts.repo.as_deref());
    let out = resolve_out(&root, opts.out.as_deref());
    let flags = run_deterministic_prechecks(&root);

    if opts.dry_run {
        write_atomic(&out, dry_run_report(&flags).as_bytes())
            .map_err(|e| CliError::new(1, format!("cannot write {}: {e}", out.display())))?;
        return Ok(out);
    }

    let block = if flags.is_empty() {
        "- none".to_string()
    } else {
        flags.join("\n")
    };
    let prompt = PROMPT_TEMPLATE.replace("__PRECHECK__", &block);

    let target = resolve_target(&root, &opts).map_err(|e| fail(&out, 1, &e))?;
    let driver = Drivers::native().get(&target.harness).ok_or_else(|| {
        fail(
            &out,
            1,
            &format!("no driver for harness '{}'", target.harness),
        )
    })?;
    let launch = driver.launch_args(&target.model, target.effort.as_deref());
    let h = headless::command(driver.herdr_kind(), launch, &prompt).ok_or_else(|| {
        fail(
            &out,
            1,
            &format!("no headless form for harness '{}'", target.harness),
        )
    })?;
    let program = opts
        .exec
        .clone()
        .unwrap_or_else(|| PathBuf::from(&h.program));
    let output = Command::new(&program)
        .current_dir(&root)
        .args(&h.args)
        .output()
        .map_err(|e| fail(&out, 1, &format!("cannot run `{}`: {e}", program.display())))?;
    let rc = output.status.code().unwrap_or(1);
    if rc != 0 || output.stdout.iter().all(u8::is_ascii_whitespace) {
        let _ = std::io::Write::write_all(&mut std::io::stderr(), &output.stderr);
        let what = if rc != 0 {
            format!("`{}` exited {rc}", h.program)
        } else {
            format!("`{}` printed no report", h.program)
        };
        return Err(fail(&out, if rc != 0 { rc } else { 1 }, &what));
    }
    write_atomic(&out, &output.stdout)
        .map_err(|e| CliError::new(1, format!("cannot write {}: {e}", out.display())))?;
    Ok(out)
}
