//! `worker prompt`: the brief contract (`--check-brief`), the watchdog's exit
//! codes and the decision that picks one, and the `worker` command that also
//! fronts `cleanup` and `stop`. The decision is a pure function of what was
//! observed, so a resumed wait reaches the same answer a first wait would.

use serde_json::json;

use super::cleanup;
use crate::cli::{CliError, Command, Env, Install, Registry as CliRegistry};
use crate::daemon::Daemon;

pub const DEFAULT_BUDGET_SEC: u64 = 2700;
pub const MAX_SEC: u64 = 7200;
pub const STALL_SEC: u64 = 600;
pub const IDLE_SEC: u64 = 120;
const BIG_TIME_MIN: u64 = 240;
const BIG_COST_USD: f64 = 10.0;
const SEVERITIES: [&str; 4] = ["critical", "major", "minor", "trivial"];

/// Exit status of `worker prompt`. The numbers are the documented contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    ReportReady = 0,
    Usage = 1,
    HardCap = 2,
    Stalled = 3,
    BudgetExceeded = 4,
    Blocked = 5,
    MalformedReport = 6,
    ProviderError = 7,
    CoreDied = 8,
}

impl Exit {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// A brief that passed `--check-brief`.
#[derive(Debug, Clone, PartialEq)]
pub struct BriefOk {
    pub budget_sec: u64,
    /// `brief`, `env` or `default`.
    pub source: &'static str,
    pub warnings: Vec<String>,
}

impl BriefOk {
    pub fn line(&self) -> String {
        format!("budget={}s source={}", self.budget_sec, self.source)
    }
}

fn at_col1<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .map(str::trim)
}

fn header_block(text: &str) -> &str {
    text.split("\n\n").next().unwrap_or("")
}

/// Validates a brief. `env_budget` is `PROMPT_WORKER_BUDGET_SEC` when set; it
/// overrides the brief's own line. An `Err` is exit 1.
pub fn check_brief(text: &str, env_budget: Option<u64>) -> Result<BriefOk, String> {
    let head = header_block(text);
    match at_col1(head, "Topology:") {
        None => return Err("brief has no 'Topology:' header".into()),
        Some(t) if !(t.starts_with("solo") || t.starts_with("lead")) => {
            return Err(format!("Topology must start with solo or lead (got '{t}')"))
        }
        Some(_) => {}
    }
    for key in ["Goal:", "Hard constraints:", "Current design"] {
        if at_col1(text, key).is_none() {
            return Err(format!("brief has no '{key}' line"));
        }
    }
    let mut warnings = Vec::new();
    match at_col1(head, "Playbook:") {
        None => warnings.push("warning: brief has no 'Playbook:' header".into()),
        Some("none") => warnings.push("warning: 'Playbook: none' needs a reason".into()),
        Some(p) if p.contains('<') => {
            warnings.push("warning: 'Playbook:' still holds a '<…>' placeholder".into())
        }
        Some(_) => {}
    }
    if at_col1(head, "Context:").is_none() {
        warnings.push("warning: brief has no 'Context:' header".into());
    }
    if let Some(v) = at_col1(text, "Must fix:") {
        if !SEVERITIES.contains(&v) {
            return Err(format!(
                "Must fix must be one of {SEVERITIES:?} (got '{v}')"
            ));
        }
    }
    if let Some(v) = at_col1(text, "Review rounds:") {
        if v.parse::<u32>().ok().filter(|n| *n > 0).is_none() {
            return Err(format!(
                "Review rounds must be a positive integer (got '{v}')"
            ));
        }
    }
    let mut from_brief = None;
    if let Some(v) = at_col1(text, "Time budget:") {
        let n = v
            .strip_suffix("min")
            .map(str::trim)
            .and_then(|n| n.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("Time budget must be '<N> min' (got '{v}')"))?;
        if n > BIG_TIME_MIN {
            warnings.push(format!(
                "warning: {n} min of time budget is a Lead-sized unit"
            ));
        }
        from_brief = Some(n * 60);
    }
    if let Some(v) = at_col1(text, "Cost budget:") {
        let c = v
            .strip_prefix('$')
            .and_then(|c| c.parse::<f64>().ok())
            .ok_or_else(|| format!("Cost budget must be '$<x>' (got '{v}')"))?;
        if c > BIG_COST_USD {
            warnings.push(format!("warning: ${c} of cost budget is a Lead-sized unit"));
        }
    }
    let (budget_sec, source) = match (env_budget, from_brief) {
        (Some(e), _) => (e, "env"),
        (None, Some(b)) => (b, "brief"),
        (None, None) => (DEFAULT_BUDGET_SEC, "default"),
    };
    Ok(BriefOk {
        budget_sec,
        source,
        warnings,
    })
}

/// What the wait observed at one poll.
#[derive(Debug, Clone, Default)]
pub struct Observed {
    pub elapsed_sec: u64,
    pub budget_sec: u64,
    /// The report text once the file exists.
    pub report: Option<String>,
    pub blocked: bool,
    pub agent_gone: bool,
    pub core_died: bool,
    pub since_change_sec: u64,
    pub idle_sec: u64,
    /// The pane reads as a provider error (consulted only on a stall or block).
    pub provider_error: bool,
}

/// A report is well-formed when it ends with the machine-read tail: a
/// `## Lessons` heading (builder) or one `Verdict:` line (reviewer).
pub fn report_well_formed(text: &str) -> bool {
    text.lines()
        .any(|l| l.trim_end() == "## Lessons" || l.starts_with("## ") && l.contains("Lessons"))
        || text.lines().any(|l| l.starts_with("Verdict: "))
}

/// The exit to take now, or `None` to keep waiting. A report beats every
/// other signal; the hard cap beats the rest.
pub fn decide(o: &Observed) -> Option<Exit> {
    if let Some(r) = &o.report {
        return Some(if report_well_formed(r) {
            Exit::ReportReady
        } else {
            Exit::MalformedReport
        });
    }
    if o.core_died {
        return Some(Exit::CoreDied);
    }
    if o.elapsed_sec >= MAX_SEC {
        return Some(Exit::HardCap);
    }
    let stalled = o.agent_gone || o.since_change_sec >= STALL_SEC || o.idle_sec >= IDLE_SEC;
    if o.blocked || stalled {
        if o.provider_error {
            return Some(Exit::ProviderError);
        }
        return Some(if o.blocked {
            Exit::Blocked
        } else {
            Exit::Stalled
        });
    }
    if o.elapsed_sec >= o.budget_sec {
        return Some(Exit::BudgetExceeded);
    }
    None
}

/// The allowance a wait runs under. `--extend` adds to it and never past the
/// hard cap (which still ends the wait with exit 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowance(pub u64);

impl Allowance {
    pub fn extend(self, sec: u64) -> Allowance {
        Allowance(self.0.saturating_add(sec))
    }
}

/// `--extend` must be a positive integer (exit 1 otherwise).
pub fn parse_extend(v: &str) -> Result<u64, String> {
    v.parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("--extend must be a positive integer (got '{v}')"))
}

fn worker_command(env: &Env, args: &[String]) -> Result<String, CliError> {
    let usage = || {
        CliError::new(1, "usage: worker prompt --check-brief <brief> | worker cleanup <name> | worker stop <name>")
    };
    match args.first().map(String::as_str) {
        Some("prompt") => match args.get(1).map(String::as_str) {
            Some("--check-brief") => {
                let text = args.get(2).ok_or_else(usage)?;
                let env_budget = std::env::var("PROMPT_WORKER_BUDGET_SEC")
                    .ok()
                    .and_then(|v| v.parse().ok());
                let ok = check_brief(text, env_budget).map_err(|e| CliError::new(1, e))?;
                let mut out = ok.warnings.join("\n");
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&ok.line());
                Ok(out)
            }
            _ => Err(usage()),
        },
        Some(verb @ ("cleanup" | "stop")) => {
            let name = args.get(1).ok_or_else(usage)?;
            let v = crate::cli::rpc_call(env, &format!("worker.{verb}"), json!({ "name": name }))?;
            Ok(v.to_string())
        }
        _ => Err(usage()),
    }
}

pub const INSTALL: Install = install;

fn install(cli: &mut CliRegistry, daemon: &mut Daemon) -> Result<(), String> {
    cli.register(Command {
        name: "worker",
        help: "prompt --check-brief | cleanup | stop",
        run: worker_command,
    })?;
    cleanup::register(daemon)
}
