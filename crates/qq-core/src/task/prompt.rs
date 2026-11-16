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

// ---- provider-error classifier (ported from the old worker/prompt/retry.rs) ----

/// Which harness's pane text shape to read (`harness.name` in config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Claude,
    Omp,
    Pi,
    Agy,
    Opencode,
}

impl Shape {
    pub fn parse(name: &str) -> Option<Shape> {
        match name {
            "claude" => Some(Shape::Claude),
            "omp" => Some(Shape::Omp),
            "pi" => Some(Shape::Pi),
            "agy" => Some(Shape::Agy),
            "opencode" => Some(Shape::Opencode),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderError {
    /// Transient: overloaded, rate limit, 5xx, network drop.
    Retryable(&'static str),
    /// Needs the human: signed out, quota or billing.
    Fatal(&'static str),
}

/// One classification, with the one thing the retry ladder still needs: has
/// the harness already spent its own retries on this failure?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub error: ProviderError,
    /// The harness printed its terminal retry line (`Retry budget exhausted
    /// after 10 retries: …`, `Retry failed after 3 attempts: …`) or its error
    /// file records at least one attempt. Its ladder is spent, so quaqua caps
    /// itself at one short retry instead of stacking a second full backoff.
    pub exhausted: bool,
}

/// Lines that open a Claude Code provider-error message (lowercased, after the
/// `⎿`/`⏺`/`●` gutter). Claude prints `API Error: …`, a bare
/// `Request timed out.`, and the login notices below; anything else on the
/// pane is the worker's own output and never classifies.
const FATAL_LINES: &[(&str, &str)] = &[
    ("not logged in", "signed-out"),
    ("invalid api key", "signed-out"),
    ("oauth token has expired", "signed-out"),
    ("please run /login", "signed-out"),
    ("credit balance is too low", "billing"),
    ("claude ai usage limit reached", "quota"),
    ("usage limit reached", "quota"),
];

/// Reason words inside an `API Error:` line. Fatal is checked first.
const API_FATAL: &[(&str, &str)] = &[
    ("authentication_error", "signed-out"),
    ("oauth token has expired", "signed-out"),
    ("invalid api key", "signed-out"),
    ("401", "signed-out"),
    ("insufficient_quota", "quota"),
    ("exceeded your current quota", "quota"),
    ("credit balance", "billing"),
];

const API_RETRYABLE: &[(&str, &str)] = &[
    ("overloaded", "overloaded"),
    ("529", "overloaded"),
    ("rate_limit", "rate-limit"),
    ("rate limit", "rate-limit"),
    ("429", "rate-limit"),
    ("timed out", "timeout"),
    ("connection error", "network"),
    ("econnreset", "network"),
    ("fetch failed", "network"),
    ("socket hang up", "network"),
    ("etimedout", "network"),
    (": 5", "5xx"),
];

fn find(table: &[(&str, &'static str)], text: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(p, _)| text.contains(p))
        .map(|(_, r)| *r)
}

/// Verdict from an HTTP status (#196: classify on the status, never on the
/// sentence). `text` only separates two reasons that are both fatal
/// (`insufficient_quota` vs a signed-out key), so a wrongly worded status can
/// never turn a retry into a fatal or back.
fn status_error(status: u16, text: &str) -> ProviderError {
    match status {
        // Connection refused / never reached the provider.
        0 => ProviderError::Retryable("network"),
        401 | 403 => {
            if text.contains("quota") || text.contains("credit") {
                ProviderError::Fatal("quota")
            } else {
                ProviderError::Fatal("signed-out")
            }
        }
        402 => ProviderError::Fatal("billing"),
        // A malformed request or an unknown model: retrying repeats it.
        400 | 404 | 422 => ProviderError::Fatal("http-error"),
        408 => ProviderError::Retryable("timeout"),
        409 => ProviderError::Retryable("conflict"),
        425 => ProviderError::Retryable("too-early"),
        429 => ProviderError::Retryable("rate-limit"),
        529 => ProviderError::Retryable("overloaded"),
        500..=599 => ProviderError::Retryable("5xx"),
        _ => ProviderError::Fatal("http-error"),
    }
}

/// Words a bare `NNN …` line must carry to be a provider error. omp's bare
/// status line has no `Error:` prefix, so the status alone would also match a
/// test line like `429 tests run, 0 failed` (rev177 F1).
const BARE_MARKERS: &[&str] = &[
    "rate limit",
    "quota",
    "api key",
    "credential",
    "overloaded",
    "unavailable",
    "timed out",
    "upstream",
    "type=",
    "param=",
];

/// Strip an optional `error: ` prefix and one optional harness terminal-retry
/// prefix. The second flag is true when the prefix was the harness saying it
/// already exhausted its own retries; the third when either prefix was there.
fn strip_lead(l: &str) -> (&str, bool, bool) {
    match l.strip_prefix("error: ") {
        Some(rest) => {
            let (rest, exhausted) = strip_terminal(rest);
            (rest, exhausted, true)
        }
        None => {
            let (rest, exhausted) = strip_terminal(l);
            (rest, exhausted, exhausted)
        }
    }
}

/// Strip `<word> after <digits> retries:/attempts: `; report whether it was
/// there.
fn strip_terminal(l: &str) -> (&str, bool) {
    for word in ["retry budget exhausted after ", "retry failed after "] {
        let Some(r) = l.strip_prefix(word) else {
            continue;
        };
        let digits = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        if digits == 0 {
            continue;
        }
        let tail = &r[digits..];
        for unit in [" retries: ", " attempts: "] {
            if let Some(t) = tail.strip_prefix(unit) {
                return (t, true);
            }
        }
    }
    (l, false)
}

/// A 3-digit status opening `l`, plus the rest of the line.
fn lead_status(l: &str) -> Option<(u16, &str)> {
    let digits = l.get(..3)?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((digits.parse().ok()?, &l[3..]))
}

/// omp (and opencode, which prints the same provider lines):
/// `[Error: ][Retry budget exhausted after N retries: ]NNN …`, plus its
/// `Error: Couldn't connect to … (connection refused).` line.
fn omp_line(l: &str) -> Option<Decision> {
    let (rest, exhausted, prefixed) = strip_lead(l);
    if rest.starts_with("couldn't connect") && rest.contains("connection refused") {
        return Some(Decision {
            error: ProviderError::Retryable("network"),
            exhausted,
        });
    }
    let (status, tail) = lead_status(rest)?;
    if !tail.starts_with(' ') || (!prefixed && !BARE_MARKERS.iter().any(|m| rest.contains(m))) {
        return None;
    }
    Some(Decision {
        error: status_error(status, rest),
        exhausted,
    })
}

/// pi: `[Error: ][Retry failed after N attempts: ]NNN: …`, or its
/// `<Label> API error (NNN)` line.
fn pi_line(l: &str) -> Option<Decision> {
    let (rest, exhausted, _) = strip_lead(l);
    if let Some((status, tail)) = lead_status(rest) {
        if tail.starts_with(": ") {
            return Some(Decision {
                error: status_error(status, rest),
                exhausted,
            });
        }
    }
    let (label, after) = rest.split_once(" api error (")?;
    if label.is_empty()
        || label.len() > 24
        || !label
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return None;
    }
    let (status, tail) = lead_status(after)?;
    tail.starts_with(')').then(|| Decision {
        error: status_error(status, rest),
        exhausted,
    })
}

/// agy's canonical status names → verdict. Classify on the name: for a gRPC
/// error the `(code N)` is the gRPC code (2 = UNKNOWN), not an HTTP status.
const AGY_STATUS: &[(&str, ProviderError)] = &[
    ("RESOURCE_EXHAUSTED", ProviderError::Retryable("rate-limit")),
    ("UNAVAILABLE", ProviderError::Retryable("5xx")),
    ("DEADLINE_EXCEEDED", ProviderError::Retryable("timeout")),
    ("INTERNAL", ProviderError::Retryable("5xx")),
    ("ABORTED", ProviderError::Retryable("conflict")),
    ("PERMISSION_DENIED", ProviderError::Fatal("signed-out")),
    ("UNAUTHENTICATED", ProviderError::Fatal("signed-out")),
    ("INVALID_ARGUMENT", ProviderError::Fatal("http-error")),
    ("NOT_FOUND", ProviderError::Fatal("http-error")),
    ("FAILED_PRECONDITION", ProviderError::Fatal("http-error")),
    ("UNKNOWN", ProviderError::Fatal("http-error")),
];

/// agy lines with no status: the credits notice and the signed-out chip.
const AGY_LITERALS: &[(&str, ProviderError)] = &[
    (
        "your ai credits balance is too low",
        ProviderError::Fatal("billing"),
    ),
    ("needs sign-in", ProviderError::Fatal("signed-out")),
];

/// agy: `⚠ <STATUS> (code NNN): <msg>` (the `Error ID:` line under it never
/// matches), or the credits line. The generic `Agent execution terminated due
/// to error.` carries no status, so it stays unclassified (`None`). A 429 or
/// 5xx code means agy already spent its own retry ladder.
fn agy_line(l: &str) -> Option<Decision> {
    let l = l.trim_start_matches(|c: char| c.is_whitespace() || c == '⚠');
    if let Some((_, error)) = AGY_LITERALS.iter().find(|(p, _)| l.starts_with(p)) {
        return Some(Decision {
            error: *error,
            exhausted: false,
        });
    }
    let (name, after) = l.split_once(" (code ")?;
    let name = name.to_ascii_uppercase();
    let mut error = AGY_STATUS.iter().find(|(s, _)| *s == name)?.1;
    let digits = after.find(|c: char| !c.is_ascii_digit())?;
    if !(1..=3).contains(&digits) || !after[digits..].starts_with("):") {
        return None;
    }
    let code: u16 = after[..digits].parse().ok()?;
    // Out of credits is a billing stop, not a rate limit to wait out.
    if name == "RESOURCE_EXHAUSTED" && (after.contains("credit") || after.contains("quota balance"))
    {
        error = ProviderError::Fatal("quota");
    }
    Some(Decision {
        error,
        exhausted: matches!(code, 429 | 500..=599),
    })
}

/// One Claude Code pane line → its provider error, if the line has the shape.
fn claude_line(l: &str) -> Option<ProviderError> {
    if l.starts_with("request timed out") {
        return Some(ProviderError::Retryable("timeout"));
    }
    if let Some(rest) = l.strip_prefix("api error") {
        return Some(
            find(API_FATAL, rest)
                .map(ProviderError::Fatal)
                .or_else(|| find(API_RETRYABLE, rest).map(ProviderError::Retryable))
                // An API error we cannot name is not known to be transient.
                .unwrap_or(ProviderError::Fatal("api-error")),
        );
    }
    FATAL_LINES
        .iter()
        .find(|(p, _)| l.starts_with(p) || (*p == "please run /login" && l.contains(p)))
        .map(|(_, r)| ProviderError::Fatal(r))
}

/// One pane line → its classification for harness `h`. The Claude tables stay
/// Claude's (#196): omp and pi print none of those shapes, and their own
/// shapes are status-first.
fn classify_line(h: Option<Shape>, line: &str) -> Option<Decision> {
    let l = line
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '⎿' | '⏺' | '●'))
        .to_lowercase();
    let claude = || {
        claude_line(&l).map(|error| Decision {
            error,
            exhausted: false,
        })
    };
    match h {
        Some(Shape::Claude) => claude(),
        Some(Shape::Omp) => omp_line(&l),
        Some(Shape::Pi) => pi_line(&l),
        Some(Shape::Agy) => agy_line(&l),
        // No kind in team.toml, and opencode (no fixture of its own yet, and it
        // had the Claude tables before #196): try each shape rather than drop
        // the error or lose a classification it used to get.
        None | Some(Shape::Opencode) => omp_line(&l).or_else(|| pi_line(&l)).or_else(claude),
    }
}

/// Classify the last `TAIL_LINES` non-empty pane lines. Fatal wins over
/// retryable: a signed-out pane that also shows an earlier overload must not
/// be retried.
pub fn classify(h: Option<Shape>, pane: &str) -> Option<Decision> {
    let lines: Vec<&str> = pane.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(TAIL_LINES)..];
    let hits: Vec<Decision> = tail.iter().filter_map(|l| classify_line(h, l)).collect();
    hits.iter()
        .find(|d| matches!(d.error, ProviderError::Fatal(_)))
        .or_else(|| hits.last())
        .copied()
}

/// Non-empty pane lines, from the bottom, read for a provider error.
pub const TAIL_LINES: usize = 15;

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
    /// Pane text shape to classify; `None` tries every shape.
    pub harness: Option<Shape>,
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

/// `runs/.<name>.error`: the structured verdict a harness extension writes,
/// one line `<class> <status> <attempts>` (`class` = `fatal | retryable`).
pub fn error_path(runs_dir: &Path, name: &str) -> PathBuf {
    side(runs_dir, name, "error")
}

fn record_error(class: &str, status: u16) -> Option<ProviderError> {
    // A record has no reason word, so a 401/403 is named neutrally.
    let by_status = match status {
        401 | 403 => ProviderError::Fatal("auth-or-quota"),
        _ => status_error(status, ""),
    };
    match (class, by_status) {
        ("retryable", ProviderError::Retryable(r)) => Some(ProviderError::Retryable(r)),
        (_, ProviderError::Fatal(r)) => Some(ProviderError::Fatal(r)),
        ("fatal", ProviderError::Retryable(_)) => Some(ProviderError::Fatal("harness-fatal")),
        _ => None,
    }
}

pub fn parse_error_record(text: &str) -> Option<Decision> {
    let mut it = text.split_whitespace();
    let class = it.next()?;
    let status: u16 = it.next()?.parse().ok()?;
    let attempts: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(Decision {
        error: record_error(class, status)?,
        exhausted: attempts > 0,
    })
}

/// Pane text after the last echoed `continue` prompt line; empty when the
/// echo is not on the pane (nothing is known to be newer than the nudge).
pub fn after_nudge(pane: &str) -> &str {
    let mut cut = None;
    let mut off = 0;
    for line in pane.split_inclusive('\n') {
        off += line.len();
        let t = line.trim().trim_start_matches(['>', '❯', '›']).trim();
        if t == "continue" {
            cut = Some(off);
        }
    }
    cut.map_or("", |i| &pane[i..])
}

/// A stalled or blocked turn's verdict: the harness record wins (and is
/// consumed, one record per decision; one older than `since` is stale), else
/// the pane tail read through `pane_read` is classified.
fn provider_verdict(
    herdr: &dyn HerdrClient,
    cfg: &WaitCfg,
    pane: Option<&str>,
    since: u64,
    nudged: bool,
) -> Option<Decision> {
    let path = error_path(cfg.runs_dir, cfg.name);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let written = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let _ = std::fs::remove_file(&path);
        if !written.is_some_and(|m| m < since) {
            if let Some(d) = parse_error_record(&text) {
                return Some(d);
            }
        }
    }
    let r = herdr.pane_read(pane?, 60).ok().filter(|r| r.ok)?;
    let text = if nudged {
        after_nudge(&r.body)
    } else {
        &r.body
    };
    classify(cfg.harness, text)
}

/// The agent's status and pane: `gone` when herdr has no such agent.
fn agent_state(herdr: &dyn HerdrClient, name: &str) -> (Option<String>, Option<String>) {
    let Ok(r) = herdr.agent_get(name) else {
        return (None, None);
    };
    if r.error_code().as_deref() == Some("agent_not_found") {
        return (Some("gone".into()), None);
    }
    let v: serde_json::Value = serde_json::from_str(&r.body).unwrap_or_default();
    let field = |k: &str| {
        v.pointer(&format!("/result/agent/{k}"))?
            .as_str()
            .map(String::from)
    };
    (field("agent_status"), field("pane_id"))
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
        let (status, pane) = agent_state(herdr, name);
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
        let mut o = Observed {
            elapsed_sec: elapsed,
            budget_sec: allowance,
            report,
            blocked: status.as_deref() == Some("blocked"),
            agent_gone: status.as_deref() == Some("gone"),
            core_died: false,
            since_change_sec: now.saturating_sub(last_change),
            idle_sec: idle_since.map_or(0, |t| now.saturating_sub(t)),
            provider_error: false,
        };
        let mut verdict = None;
        if matches!(decide(&o), Some(Exit::Stalled | Exit::Blocked)) {
            verdict = provider_verdict(herdr, cfg, pane.as_deref(), start, attempt > 0);
            o.provider_error = verdict.is_some();
        }
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
                let d = verdict.expect("decide sets ProviderError only with a verdict");
                let (reason, retryable) = match d.error {
                    ProviderError::Retryable(r) => (r, true),
                    ProviderError::Fatal(r) => (r, false),
                };
                attempt += 1;
                let cap = if d.exhausted { 1 } else { cfg.retry_max };
                if !retryable || attempt > cap {
                    (cfg.journal)(
                        "provider_escalate",
                        json!({"reason": reason, "retryable": retryable, "retries": attempt - 1, "harness_exhausted": d.exhausted}),
                    );
                    let _ = std::fs::remove_file(&retry_f);
                    return Exit::ProviderError;
                }
                let delay = cfg.retry_base_ms.saturating_mul(1 << (attempt - 1).min(20));
                (cfg.journal)(
                    "provider_retry",
                    json!({"attempt": attempt, "reason": reason, "delay_ms": delay, "harness_exhausted": d.exhausted}),
                );
                write_atomic(&retry_f, &format!("{now}\n"));
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

/// Poll and retry timing; the CLI reads the `PROMPT_WORKER_*` env, tests pass their own.
pub struct Tuning {
    pub poll_ms: u64,
    pub retry_base_ms: u64,
    pub retry_max: u32,
}

impl Tuning {
    pub fn from_env() -> Tuning {
        Tuning {
            poll_ms: env_u64("PROMPT_WORKER_POLL_MS", 5_000),
            retry_base_ms: env_u64("PROMPT_WORKER_RETRY_BASE_MS", 60_000),
            retry_max: env_u64("PROMPT_WORKER_RETRY_MAX", 4) as u32,
        }
    }
}

/// The brief plus the report instruction, as the old binary composed it.
pub fn compose_prompt_body(name: &str, task: &str) -> String {
    format!(
        "{task}. When done, write summary (changes, test cmd+result, risks) to a temp file and rename it to ~/quaqua/runs/{name}.md (publish it atomically, never a partial file); reply only the path."
    )
}

/// Sends the brief through herdr and checks it reached the turn. A stalled
/// submission gets exactly one enter and one re-wait (as the old binary did).
/// Exit 5: the agent is blocked on a question; exit 1: it never landed.
fn send_brief(
    herdr: &dyn HerdrClient,
    runs: &Path,
    name: &str,
    body: &str,
) -> Result<(), CliError> {
    let landed = |r: &crate::herdr::Reply| r.ok;
    let first = herdr.agent_prompt(name, body, Some((&["working", "blocked"], 10_000)));
    if let Ok(r) = &first {
        let _ = std::fs::write(side(runs, name, "submit.log"), &r.body);
        if r.error_code().as_deref() == Some("agent_blocked") {
            return Err(CliError::new(
                5,
                format!("WATCHDOG exit=5 blocked before the brief | name={name}"),
            ));
        }
        if landed(r) {
            return Ok(());
        }
    }
    eprintln!("WATCHDOG prompt-not-landed | name={name} - sent enter once");
    let _ = herdr.agent_send_keys(name, &["enter"]);
    match herdr.agent_wait(name, &["working", "blocked"], 10_000) {
        Ok(r) if landed(&r) => Ok(()),
        _ => Err(CliError::new(
            1,
            format!("the brief did not land | name={name}"),
        )),
    }
}

/// The worktree and harness shape of an open agent, from the database.
fn lookup(runs: &Path, name: &str) -> (Option<PathBuf>, Option<Shape>) {
    let Ok(c) = crate::db::open(&runs.join(crate::db::path::DB_FILE)) else {
        return (None, None);
    };
    let wt = c
        .query_row(
            "SELECT p.repo_path FROM agent a JOIN project p ON p.id = a.project_id WHERE a.name = ?1 AND a.ended_ms IS NULL",
            [name],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .map(|r| {
            let r = PathBuf::from(r);
            r.parent().unwrap_or(&r).join(".wt").join(name)
        });
    let shape = c
        .query_row(
            "SELECT k.harness FROM agent a JOIN kind k ON k.name = a.kind WHERE a.name = ?1 AND a.ended_ms IS NULL",
            [name],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|h| Shape::parse(&h));
    (wt, shape)
}

/// `worker prompt <name> "<brief>" [--budget SEC]` sends the brief, then
/// waits; `worker prompt <name> --resume [--extend SEC]` only waits again.
pub fn prompt_run(
    herdr: &dyn HerdrClient,
    clock: &dyn Clock,
    runs: &Path,
    args: &[String],
    tuning: &Tuning,
) -> Result<String, CliError> {
    let usage = || {
        CliError::new(1, "usage: worker prompt <name> \"<brief>\" [--budget SEC] | <name> --resume [--extend SEC] | --check-brief <brief>")
    };
    let name = args
        .first()
        .filter(|n| !n.starts_with("--"))
        .ok_or_else(usage)?;
    let (mut brief, mut budget, mut extend, mut resume) = (None, None, None, false);
    let mut it = args[1..].iter();
    let num =
        |v: Option<&String>| parse_extend(v.ok_or_else(usage)?).map_err(|e| CliError::new(1, e));
    while let Some(a) = it.next() {
        match a.as_str() {
            "--resume" => resume = true,
            "--budget" => budget = Some(num(it.next())?),
            "--extend" => extend = Some(num(it.next())?),
            t if !t.starts_with("--") && brief.is_none() => brief = Some(t),
            _ => return Err(usage()),
        }
    }
    if brief.is_some() == resume {
        return Err(usage()); // a fresh prompt needs a brief; a resume must not carry one
    }
    let _ = std::fs::create_dir_all(runs);
    let budget_f = side(runs, name, "budget");
    let mut brief_budget = None;
    if let Some(text) = brief {
        let env_budget = std::env::var("PROMPT_WORKER_BUDGET_SEC")
            .ok()
            .and_then(|v| v.parse().ok());
        let ok = check_brief(text, env_budget).map_err(|e| CliError::new(1, e))?;
        for w in &ok.warnings {
            eprintln!("{w}");
        }
        brief_budget = Some(ok.budget_sec);
        // A new task starts clean: no counter, error record or pin of an earlier one.
        for ext in [
            "budget",
            "grace",
            "autoext",
            "nudged",
            "elapsed",
            "provider_retry",
            "error",
            "playbook",
        ] {
            let _ = std::fs::remove_file(side(runs, name, ext));
        }
        send_brief(herdr, runs, name, &compose_prompt_body(name, text))?;
    }
    if let Some(sec) = extend {
        let cur = read_u64(&budget_f)
            .ok_or_else(|| CliError::new(1, format!("no persisted budget for '{name}'")))?;
        write_atomic(&budget_f, &format!("{}\n", Allowance(cur).extend(sec).0));
        eprintln!("WATCHDOG extend +{sec}s");
    }
    let (worktree, harness) = lookup(runs, name);
    let report = runs.join(format!("{name}.md"));
    let journal = journal_to_db(runs, name);
    let cfg = WaitCfg {
        name,
        runs_dir: runs,
        report: report.clone(),
        worktree,
        budget_sec: budget.or(brief_budget).unwrap_or(DEFAULT_BUDGET_SEC),
        poll_ms: tuning.poll_ms,
        retry_base_ms: tuning.retry_base_ms,
        retry_max: tuning.retry_max,
        harness,
        journal: &journal,
    };
    match wait(herdr, clock, &cfg) {
        Exit::ReportReady => Ok(report.display().to_string()),
        e => Err(CliError::new(
            e.code(),
            format!("WATCHDOG exit={} | name={name}", e.code()),
        )),
    }
}

fn worker_command(env: &Env, args: &[String]) -> Result<String, CliError> {
    let usage = || {
        CliError::new(1, "usage: worker prompt <name> \"<brief>\" | prompt <name> --resume | prompt --check-brief <brief> | worker cleanup <name> | worker stop <name>")
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
            _ => prompt_run(
                &crate::herdr::CliHerdr::new(),
                &SysClock,
                &env.runs_dir,
                &args[1..],
                &Tuning::from_env(),
            ),
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
