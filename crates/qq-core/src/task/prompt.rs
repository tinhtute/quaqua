//! `worker prompt`: the brief contract (`--check-brief`), the watchdog's exit
//! codes and the decision that picks one, and the `worker` command that also
//! fronts `cleanup` and `stop`. The decision is a pure function of what was
//! observed, so a resumed wait reaches the same answer a first wait would.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::herdr::HerdrClient;

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

// ---- the live wait -------------------------------------------------------

/// Time source and sleeper; tests drive a fake one.
pub trait Clock {
    fn now_sec(&self) -> u64;
    fn sleep_ms(&self, ms: u64);
}

pub struct SysClock;

impl Clock for SysClock {
    fn now_sec(&self) -> u64 {
        crate::engine::clock::SystemClock.now_ms() / 1000
    }
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// Where a wait reads and writes. `journal` records `provider_*` events.
pub struct WaitCfg<'a> {
    pub name: &'a str,
    pub runs_dir: &'a Path,
    pub report: PathBuf,
    pub worktree: Option<PathBuf>,
    /// The brief's budget; a persisted `.budget` (resume, `--extend`) wins.
    pub budget_sec: u64,
    pub poll_ms: u64,
    pub retry_base_ms: u64,
    pub retry_max: u32,
    pub journal: &'a dyn Fn(&str, serde_json::Value),
}

fn side(runs: &Path, name: &str, ext: &str) -> PathBuf {
    runs.join(format!(".{name}.{ext}"))
}

fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_atomic(path: &Path, text: &str) {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// A hash of the worktree's uncommitted state, or empty without one.
fn diff_hash(worktree: &Option<PathBuf>) -> String {
    use std::hash::{Hash, Hasher};
    let Some(wt) = worktree else {
        return String::new();
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for args in [["status", "--porcelain"], ["diff", "HEAD"]] {
        if let Ok(o) = std::process::Command::new("git")
            .arg("-C")
            .arg(wt)
            .args(args)
            .output()
        {
            o.stdout.hash(&mut h);
        }
    }
    format!("{:x}", h.finish())
}

/// The harness verdict a stalled turn left in `.<name>.error`: first line is
/// the reason, a `fatal:` prefix marks it non-retryable. herdr's client has no
/// pane read, so the pane is never scraped.
fn provider_verdict(runs: &Path, name: &str) -> Option<(String, bool)> {
    let text = std::fs::read_to_string(side(runs, name, "error")).ok()?;
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    Some(match line.strip_prefix("fatal:") {
        Some(r) => (r.trim().to_string(), false),
        None => (line.to_string(), true),
    })
}

fn agent_status(herdr: &dyn HerdrClient, name: &str) -> Option<String> {
    let r = herdr.agent_get(name).ok()?;
    if r.error_code().as_deref() == Some("agent_not_found") {
        return Some("gone".into());
    }
    let v: serde_json::Value = serde_json::from_str(&r.body).ok()?;
    v.pointer("/result/agent/agent_status")?
        .as_str()
        .map(String::from)
}

/// Waits for `cfg.report`; returns the exit to take. Re-entrant: every
/// counter lives in `runs/.<name>.*`, so `--resume` continues the same wait.
pub fn wait(herdr: &dyn HerdrClient, clock: &dyn Clock, cfg: &WaitCfg) -> Exit {
    let (runs, name) = (cfg.runs_dir, cfg.name);
    let (budget_f, grace_f, auto_f) = (
        side(runs, name, "budget"),
        side(runs, name, "grace"),
        side(runs, name, "autoext"),
    );
    let (nudged_f, elapsed_f, retry_f) = (
        side(runs, name, "nudged"),
        side(runs, name, "elapsed"),
        side(runs, name, "provider_retry"),
    );
    let base = cfg.budget_sec.max(1);
    let mut allowance = read_u64(&budget_f).unwrap_or(base);
    write_atomic(&budget_f, &format!("{allowance}\n"));
    let start = clock
        .now_sec()
        .saturating_sub(read_u64(&elapsed_f).unwrap_or(0));
    let mut last_change = clock.now_sec();
    let mut idle_since: Option<u64> = None;
    let mut fingerprint = String::new();
    let mut last_auto_hash = String::new();
    let mut attempt: u32 = 0;
    loop {
        let now = clock.now_sec();
        let elapsed = now.saturating_sub(start);
        write_atomic(&elapsed_f, &format!("{elapsed}\n"));
        let report = std::fs::read_to_string(&cfg.report).ok();
        let status = agent_status(herdr, name);
        let hash = diff_hash(&cfg.worktree);
        let fp = format!("{}|{hash}", status.as_deref().unwrap_or("?"));
        if fp != fingerprint {
            fingerprint = fp;
            last_change = now;
        }
        match status.as_deref() {
            Some("idle" | "done") => {
                idle_since.get_or_insert(now);
            }
            _ => idle_since = None,
        }
        let verdict = provider_verdict(runs, name);
        let o = Observed {
            elapsed_sec: elapsed,
            budget_sec: allowance,
            report,
            blocked: status.as_deref() == Some("blocked"),
            agent_gone: status.as_deref() == Some("gone"),
            core_died: false,
            since_change_sec: now.saturating_sub(last_change),
            idle_sec: idle_since.map_or(0, |t| now.saturating_sub(t)),
            provider_error: verdict.is_some(),
        };
        if o.report.is_none() && !nudged_f.exists() && elapsed * 100 >= allowance * 80 {
            let msg = format!(
                "Time check: you are past 80% of this task's time budget. Wrap up now: finish the current step, commit your work, and write {} (via a temp file + rename) with what is done plus a Blockers section for the rest. Reply only the path.",
                cfg.report.display()
            );
            let _ = herdr.agent_prompt(name, &msg, None);
            write_atomic(&nudged_f, "");
        }
        let Some(exit) = decide(&o) else {
            clock.sleep_ms(cfg.poll_ms);
            continue;
        };
        match exit {
            Exit::BudgetExceeded => {
                let room = MAX_SEC.saturating_sub(allowance);
                let progressing = o.since_change_sec <= 2 * cfg.poll_ms / 1000 + 1;
                if read_u64(&grace_f).is_none() && progressing && room > 0 {
                    allowance += (base / 4).max(1).min(room);
                    write_atomic(&grace_f, "1\n");
                } else {
                    let used = read_u64(&auto_f).unwrap_or(0);
                    let step = (base / 4)
                        .max(1)
                        .min(base / 2 - used.min(base / 2))
                        .min(room);
                    if step > 0 && !hash.is_empty() && hash != last_auto_hash {
                        last_auto_hash = hash;
                        allowance += step;
                        write_atomic(&auto_f, &format!("{}\n", used + step));
                    } else {
                        return Exit::BudgetExceeded;
                    }
                }
                write_atomic(&budget_f, &format!("{allowance}\n"));
            }
            Exit::ProviderError => {
                let (reason, retryable) = verdict.unwrap_or_default();
                attempt += 1;
                if !retryable || attempt > cfg.retry_max {
                    (cfg.journal)(
                        "provider_escalate",
                        json!({"reason": reason, "retryable": retryable, "retries": attempt - 1}),
                    );
                    let _ = std::fs::remove_file(&retry_f);
                    return Exit::ProviderError;
                }
                let delay = cfg.retry_base_ms.saturating_mul(1 << (attempt - 1).min(20));
                (cfg.journal)(
                    "provider_retry",
                    json!({"attempt": attempt, "reason": reason, "delay_ms": delay}),
                );
                write_atomic(&retry_f, &format!("{now}\n"));
                let _ = std::fs::remove_file(side(runs, name, "error"));
                clock.sleep_ms(delay);
                let _ = herdr.agent_prompt(name, "continue", None);
                last_change = clock.now_sec();
                idle_since = None;
            }
            other => {
                let _ = std::fs::remove_file(&retry_f);
                return other;
            }
        }
    }
}

fn journal_to_db(runs: &Path, name: &str) -> impl Fn(&str, serde_json::Value) {
    let (runs, name) = (runs.to_path_buf(), name.to_string());
    move |kind, payload| {
        use crate::engine::journal::{EventRecord, Journal};
        let res = crate::engine::journal_sqlite::SqliteJournal::open(
            &runs.join(crate::db::path::DB_FILE),
        )
        .and_then(|mut j| {
            let ts = crate::engine::clock::SystemClock.now_ms();
            j.append_event(
                &format!("worker:{name}"),
                &EventRecord::new(0, ts, kind, payload.to_string()),
            )
        });
        if let Err(e) = res {
            eprintln!("warning: cannot record {kind} in the journal: {e}");
        }
    }
}

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

/// `worker prompt <name> [--budget SEC] [--resume] [--extend SEC]`.
fn prompt_wait(env: &Env, args: &[String]) -> Result<String, CliError> {
    let usage = || {
        CliError::new(1, "usage: worker prompt <name> [--budget SEC] [--resume] [--extend SEC] | --check-brief <brief>")
    };
    let name = args
        .first()
        .filter(|n| !n.starts_with("--"))
        .ok_or_else(usage)?;
    let (mut budget, mut extend) = (None, None);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--resume" => {}
            "--budget" => {
                budget = Some(
                    parse_extend(it.next().ok_or_else(usage)?).map_err(|e| CliError::new(1, e))?,
                )
            }
            "--extend" => {
                extend = Some(
                    parse_extend(it.next().ok_or_else(usage)?).map_err(|e| CliError::new(1, e))?,
                )
            }
            _ => return Err(usage()),
        }
    }
    let runs = &env.runs_dir;
    let budget_f = side(runs, name, "budget");
    if let Some(sec) = extend {
        let cur = read_u64(&budget_f)
            .ok_or_else(|| CliError::new(1, format!("no persisted budget for '{name}'")))?;
        let next = Allowance(cur).extend(sec).0;
        write_atomic(&budget_f, &format!("{next}\n"));
        eprintln!("WATCHDOG extend +{sec}s");
    }
    let brief = budget
        .or_else(|| {
            std::env::var("PROMPT_WORKER_BUDGET_SEC")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(DEFAULT_BUDGET_SEC);
    let worktree = crate::db::open(&runs.join(crate::db::path::DB_FILE)).ok().and_then(|c| {
        let repo: Option<String> = c
            .query_row(
                "SELECT p.repo_path FROM agent a JOIN project p ON p.id = a.project_id WHERE a.name = ?1 AND a.ended_ms IS NULL",
                [name],
                |r| r.get(0),
            )
            .ok();
        repo.map(|r| {
            let r = PathBuf::from(r);
            r.parent().unwrap_or(&r).join(".wt").join(name)
        })
    });
    let report = runs.join(format!("{name}.md"));
    let journal = journal_to_db(runs, name);
    let cfg = WaitCfg {
        name,
        runs_dir: runs,
        report: report.clone(),
        worktree,
        budget_sec: brief,
        poll_ms: env_u64("PROMPT_WORKER_POLL_MS", 5_000),
        retry_base_ms: env_u64("PROMPT_WORKER_RETRY_BASE_MS", 60_000),
        retry_max: env_u64("PROMPT_WORKER_RETRY_MAX", 4) as u32,
        journal: &journal,
    };
    let herdr = crate::herdr::CliHerdr::new();
    match wait(&herdr, &SysClock, &cfg) {
        Exit::ReportReady => Ok(report.display().to_string()),
        e => Err(CliError::new(
            e.code(),
            format!("WATCHDOG exit={} | name={name}", e.code()),
        )),
    }
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
            _ => prompt_wait(env, &args[1..]),
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
