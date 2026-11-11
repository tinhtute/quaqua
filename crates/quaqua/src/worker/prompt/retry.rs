//! Provider-error retry (#177, #196): classify a turn that ended without a
//! report from the worker's structured error file, else from its pane text;
//! retry a transient provider error with capped exponential backoff, and
//! escalate the rest.
//!
//! Knobs: `PROMPT_WORKER_RETRY_BASE_MS` (first delay, default 60000 = 1 min;
//! delays double: 1, 2, 4, 8 min) and `PROMPT_WORKER_RETRY_MAX` (default 4).

use crate::engine::journal::{EventRecord, Journal};
use crate::engine::journal_sqlite::SqliteJournal;
use crate::harness::Harness;
use crate::util::now_sec;
use std::path::{Path, PathBuf};

/// Watchdog exit code for a provider error that needs a human: a
/// non-retryable one, or a retryable one past `PROMPT_WORKER_RETRY_MAX`.
pub const EXIT_PROVIDER: i32 = 7;

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

/// The worker being classified: which harness's text shape to read, and where
/// its `runs/.<name>.error` record lives.
#[derive(Debug, Clone, Copy)]
pub struct Worker<'a> {
    pub name: &'a str,
    pub runs_dir: &'a Path,
    /// `None` when team.toml does not name this worker's kind: every harness
    /// shape is tried then, rather than losing the classification.
    pub harness: Option<Harness>,
    /// Epoch second the turn being watched started. A record older than this
    /// belongs to an earlier turn (the harness failed it before quaqua began
    /// watching) and is never trusted; 0 trusts any record.
    pub since: u64,
}

/// `runs/.<name>.error`: the structured verdict the harness extension writes
/// (#195/#196), one line `<class> <status> <attempts>` with `class` in
/// `fatal | retryable`, `status` an HTTP status (0 = connection refused) and
/// `attempts` the harness's own retry count.
pub fn error_path(runs_dir: &Path, name: &str) -> PathBuf {
    runs_dir.join(format!(".{name}.error"))
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

/// The harness of worker `name` from `team.toml` (kind → harness), the same
/// lookup `cost.rs` does. `None` when the config is unreadable or names no
/// such kind.
pub fn harness_of(name: &str) -> Option<Harness> {
    let cfg = crate::config::load_team_config().ok()?;
    let kind = crate::config::kind_of_worker(&cfg, name)?;
    Harness::parse(&cfg.kinds.get(kind)?.harness)
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
fn classify_line(h: Option<Harness>, line: &str) -> Option<Decision> {
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
        Some(Harness::Claude) => claude(),
        Some(Harness::Omp) => omp_line(&l),
        Some(Harness::Pi) => pi_line(&l),
        // agy: no provider-error fixture of its own yet, and porting another
        // harness's table would classify text agy never prints (rules/_all
        // harness adapters), so it stays unclassified until a real pane shows
        // the shape.
        Some(Harness::Agy) => None,
        // No kind in team.toml, and opencode (no fixture of its own yet, and it
        // had the Claude tables before #196): try each shape rather than drop
        // the error or lose a classification it used to get.
        None | Some(Harness::Opencode) => omp_line(&l).or_else(|| pi_line(&l)).or_else(claude),
    }
}

/// Classify the last `TAIL_LINES` non-empty pane lines. Fatal wins over
/// retryable: a signed-out pane that also shows an earlier overload must not
/// be retried.
pub fn classify(h: Option<Harness>, pane: &str) -> Option<Decision> {
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

/// How a `prompt_worker_once` run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The brief landed and the watchdog ran to an exit.
    Watched,
    /// The watchdog interrupted the worker at its cost cap (#48).
    CostCap,
    /// The brief never reached the turn (submit blocked / not landed).
    NotLanded,
}

/// The retry decision for one `prompt_worker_once` result: only a watched
/// turn that ended stalled (3) or blocked (5) qualifies — never a cost-cap
/// kill or a brief that never landed.
///
/// The harness's structured record wins over its text when present, and is
/// consumed by the read so a stale record cannot shadow a newer pane line
/// after quaqua's own retry. After a `continue` nudge (`nudged`), only the
/// pane text below the echoed nudge counts.
pub fn decide(
    code: i32,
    ended: Ended,
    pane: &str,
    nudged: bool,
    worker: &Worker,
) -> Option<Decision> {
    if !matches!(code, 3 | 5) || ended != Ended::Watched {
        return None;
    }
    if let Some(d) = read_error_file(worker.runs_dir, worker.name, worker.since) {
        return Some(d);
    }
    let text = if nudged { after_nudge(pane) } else { pane };
    classify(worker.harness, text)
}

/// Parse `runs/.<name>.error` into a decision, then delete it: one record
/// answers one decision (idempotent to re-run — a second call finds nothing).
/// A record written before the turn start is stale: the harness produced it
/// for an earlier turn, so it is dropped and the text decides.
fn read_error_file(runs_dir: &Path, name: &str, since: u64) -> Option<Decision> {
    let path = error_path(runs_dir, name);
    let written = mtime_sec(&path);
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    if written.is_some_and(|m| m < since) {
        return None;
    }
    parse_error_record(&text)
}

/// `path`'s mtime in epoch seconds, when the filesystem reports one.
fn mtime_sec(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn parse_error_record(text: &str) -> Option<Decision> {
    let mut it = text.split_whitespace();
    let class = it.next()?;
    let status: u16 = it.next()?.parse().ok()?;
    let attempts: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(Decision {
        error: record_error(class, status)?,
        // The harness retried `attempts` times itself; its ladder is spent.
        exhausted: attempts > 0,
    })
}

fn record_error(class: &str, status: u16) -> Option<ProviderError> {
    // A record carries no reason word, so a 401/403 cannot be told apart as a
    // signed-out key or an exhausted quota: name it neutrally rather than
    // claim one.
    let by_status = match status {
        401 | 403 => ProviderError::Fatal("auth-or-quota"),
        _ => status_error(status, ""),
    };
    match (class, by_status) {
        ("retryable", ProviderError::Retryable(r)) => Some(ProviderError::Retryable(r)),
        // Fail closed: a status that needs a human is never retried, and a
        // record the writer already called fatal is never retried either.
        (_, ProviderError::Fatal(r)) => Some(ProviderError::Fatal(r)),
        ("fatal", ProviderError::Retryable(_)) => Some(ProviderError::Fatal("harness-fatal")),
        _ => None,
    }
}

/// How many retries a classification earns: the full ladder, or one short
/// retry when the harness already spent its own (#196 retry stacking).
pub fn retry_cap(d: &Decision) -> u32 {
    if d.exhausted {
        1
    } else {
        max_retries()
    }
}

/// Pane text after the last echoed `continue` prompt line; empty when the
/// echo is not on the pane (nothing is known to be newer than the nudge).
fn after_nudge(pane: &str) -> &str {
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

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

pub fn max_retries() -> u32 {
    env_u64("PROMPT_WORKER_RETRY_MAX", 4) as u32
}

/// Delay before retry `attempt` (1-based): base, 2×base, 4×base, …
pub fn backoff_ms(attempt: u32) -> u64 {
    let base = env_u64("PROMPT_WORKER_RETRY_BASE_MS", 60_000);
    base.saturating_mul(1u64 << attempt.saturating_sub(1).min(20))
}

/// Append one event to the worker's journal stream; a journal failure warns
/// and never changes the outcome.
pub fn journal(runs_dir: &Path, name: &str, kind: &str, payload: serde_json::Value) {
    let res = SqliteJournal::open(&runs_dir.join("quaqua.db")).and_then(|mut j| {
        let ev = EventRecord::new(0, now_sec().saturating_mul(1000), kind, payload.to_string());
        j.append_event(&format!("worker:{name}"), &ev)
    });
    if let Err(e) = res {
        eprintln!("warning: cannot record {kind} in the journal: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decision with no harness-exhaustion flag, for the fixture tables.
    fn hit(error: ProviderError) -> Option<Decision> {
        Some(Decision {
            error,
            exhausted: false,
        })
    }

    /// The verbatim omp 18.13.3 lines from #196, each with the class it owes.
    #[test]
    fn classify_omp_lines_by_status() {
        for (text, want) in [
            (
                "Error: 429 Rate limit reached for gpt-5.1-codex-mini on requests per min (RPM): ... (type=requests param=rate_limit_exceeded)",
                ProviderError::Retryable("rate-limit"),
            ),
            (
                "401 Incorrect API key provided: sk-fake. ... (type=invalid_request_error param=invalid_api_key)",
                ProviderError::Fatal("signed-out"),
            ),
            (
                "403 You exceeded your current quota, ... (type=insufficient_quota param=insufficient_quota)",
                ProviderError::Fatal("quota"),
            ),
            (
                "401 Upstream request failed: Invalid credential (type=server_error)",
                ProviderError::Fatal("signed-out"),
            ),
            (
                "Error: Couldn't connect to 127.0.0.1:8899 (connection refused). Check that the server is running and reachable.",
                ProviderError::Retryable("network"),
            ),
        ] {
            assert_eq!(classify(Some(Harness::Omp), text), hit(want), "{text}");
        }
    }

    /// The verbatim pi 1.4.0 lines from #196.
    #[test]
    fn classify_pi_lines_by_status() {
        for (text, want) in [
            (
                "Error: 429: {\"message\":\"Rate limit reached ...\",\"code\":\"rate_limit_exceeded\"}",
                ProviderError::Retryable("rate-limit"),
            ),
            (
                "OpenAI API error (429): {\"message\":\"Rate limit reached\"}",
                ProviderError::Retryable("rate-limit"),
            ),
            (
                "401: {\"message\":\"Incorrect API key provided: ...\",\"code\":\"invalid_api_key\"}",
                ProviderError::Fatal("signed-out"),
            ),
            (
                "403: {\"message\":\"You exceeded your current quota, ...\",\"code\":\"insufficient_quota\"}",
                ProviderError::Fatal("quota"),
            ),
            (
                "529: {\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}",
                ProviderError::Retryable("overloaded"),
            ),
        ] {
            assert_eq!(classify(Some(Harness::Pi), text), hit(want), "{text}");
        }
    }

    /// The harness's own terminal line means its retries are spent: still
    /// classified, but flagged so quaqua does not stack a second ladder.
    #[test]
    fn classify_marks_a_spent_harness_ladder_as_exhausted() {
        let omp = "Retry budget exhausted after 10 retries: 429 Rate limit reached ...";
        assert_eq!(
            classify(Some(Harness::Omp), omp),
            Some(Decision {
                error: ProviderError::Retryable("rate-limit"),
                exhausted: true,
            })
        );
        let pi = "Error: Retry failed after 3 attempts: 429: {\"message\":\"Rate limit reached\"}";
        assert_eq!(
            classify(Some(Harness::Pi), pi),
            Some(Decision {
                error: ProviderError::Retryable("rate-limit"),
                exhausted: true,
            })
        );
        // The request that was never retried is not exhausted.
        assert_eq!(
            classify(Some(Harness::Pi), "Error: 429: {}"),
            hit(ProviderError::Retryable("rate-limit"))
        );
        // One short retry, not the full ladder.
        let d = classify(Some(Harness::Omp), omp).unwrap();
        assert_eq!(retry_cap(&d), 1);
        assert_eq!(
            retry_cap(&classify(Some(Harness::Pi), "Error: 429: {}").unwrap()),
            max_retries()
        );
    }

    /// #196: the Claude tables stay Claude's; no harness borrows another's.
    #[test]
    fn classify_keeps_the_claude_tables_for_claude() {
        for (text, want) in [
            (
                "API Error: 529 Overloaded",
                ProviderError::Retryable("overloaded"),
            ),
            (
                "  ⎿  API Error: 429 rate_limit_error",
                ProviderError::Retryable("rate-limit"),
            ),
            (
                "API Error: 503 Service Unavailable",
                ProviderError::Retryable("5xx"),
            ),
            (
                "API Error: Connection error (ECONNRESET)",
                ProviderError::Retryable("network"),
            ),
            ("Request timed out.", ProviderError::Retryable("timeout")),
            (
                "⎿  API Error: Request timed out.",
                ProviderError::Retryable("timeout"),
            ),
            (
                "Not logged in · Please run /login",
                ProviderError::Fatal("signed-out"),
            ),
            (
                "OAuth token has expired. Please obtain a new token or run /login.",
                ProviderError::Fatal("signed-out"),
            ),
            ("Credit balance is too low", ProviderError::Fatal("billing")),
            (
                "API Error: 529 Overloaded\n\nInvalid API key · Please run /login",
                ProviderError::Fatal("signed-out"),
            ),
        ] {
            assert_eq!(classify(Some(Harness::Claude), text), hit(want), "{text}");
        }
        assert_eq!(
            classify(
                Some(Harness::Claude),
                "I finished the task and wrote the report."
            ),
            None
        );
        // Claude's tables are not ported: a claude-shaped line on an omp pane
        // does not classify, and an omp line on a claude pane does not either.
        assert_eq!(
            classify(Some(Harness::Omp), "API Error: 529 Overloaded"),
            None
        );
        assert_eq!(
            classify(Some(Harness::Claude), "Error: 429 Rate limit reached"),
            None
        );
    }

    /// A worker whose kind team.toml does not name still classifies: every
    /// shape is tried, in that order, instead of silently losing the error.
    #[test]
    fn classify_falls_back_to_every_shape_without_a_kind() {
        for (text, want) in [
            (
                "Error: 503 Service Unavailable",
                ProviderError::Retryable("5xx"),
            ),
            (
                "529: {\"type\":\"overloaded_error\"}",
                ProviderError::Retryable("overloaded"),
            ),
            (
                "API Error: 529 Overloaded",
                ProviderError::Retryable("overloaded"),
            ),
            (
                "Not logged in · Please run /login",
                ProviderError::Fatal("signed-out"),
            ),
        ] {
            assert_eq!(classify(None, text), hit(want), "{text}");
        }
        assert_eq!(classify(None, "429 tests run, 0 failed"), None);
    }

    /// rev177 F1: the worker's own output that mentions a table word is not
    /// a provider error.
    #[test]
    fn classify_ignores_free_text_that_only_mentions_provider_words() {
        for pane in [
            "Edit crates/billing/src/invoice.rs?\n❯ 1. Yes  2. No",
            "Allow Bash(cargo test -p ratelimit -- rate limit)?\n1. Yes 2. No",
            "warning: method `fmt` is overloaded here\nDo you want to proceed?",
            "⏺ Done. Added retry on `fetch failed`; report pending.",
            "429 tests run, 0 failed",
            "The mock server answered 401 to the third request",
        ] {
            for h in [Some(Harness::Claude), Some(Harness::Omp), Some(Harness::Pi)] {
                assert_eq!(classify(h, pane), None, "{h:?} {pane}");
            }
        }
    }

    /// Only the tail of the pane counts: an error far above the last lines
    /// is history, not the reason the turn stopped.
    #[test]
    fn classify_reads_only_the_last_lines() {
        let mut pane = String::from("Error: 503 Service Unavailable\n");
        for i in 0..TAIL_LINES {
            pane.push_str(&format!("line {i}\n"));
        }
        for h in [Some(Harness::Claude), Some(Harness::Omp), Some(Harness::Pi)] {
            assert_eq!(classify(h, &pane), None, "{h:?}");
        }
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "quaqua-retry-{}-{}-{tag}",
                std::process::id(),
                now_sec()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn error_record_wins_over_pane_text_and_is_consumed_once() {
        let dir = TempDir::new("record");
        let w = Worker {
            name: "sude-x",
            runs_dir: &dir.0,
            harness: Some(Harness::Omp),
            since: 0,
        };
        std::fs::write(error_path(&dir.0, "sude-x"), "fatal 401 0\n").unwrap();
        // The pane says retryable; the structured record says a human is needed.
        let pane = "Error: 429 Rate limit reached";
        assert_eq!(
            decide(3, Ended::Watched, pane, false, &w),
            Some(Decision {
                error: ProviderError::Fatal("auth-or-quota"),
                exhausted: false,
            })
        );
        // One record answers one decision: the second call reads the text.
        assert_eq!(
            decide(3, Ended::Watched, pane, false, &w),
            hit(ProviderError::Retryable("rate-limit"))
        );
    }

    #[test]
    fn error_record_attempts_flag_a_spent_ladder() {
        let dir = TempDir::new("attempts");
        let w = Worker {
            name: "w",
            runs_dir: &dir.0,
            harness: Some(Harness::Pi),
            since: 0,
        };
        std::fs::write(error_path(&dir.0, "w"), "retryable 429 3\n").unwrap();
        let d = decide(5, Ended::Watched, "", false, &w).unwrap();
        assert_eq!(d.error, ProviderError::Retryable("rate-limit"));
        assert!(d.exhausted);
        assert_eq!(retry_cap(&d), 1);
    }

    #[test]
    fn malformed_error_record_falls_back_to_text() {
        let pane = "Error: 503 Service Unavailable";
        for record in [
            "fatal 401",
            "fatal 4xx 1",
            "fatal 401 1 extra",
            "bogus 429 1",
            "retryable 0x1 1",
        ] {
            let dir = TempDir::new("malformed");
            let w = Worker {
                name: "w",
                runs_dir: &dir.0,
                harness: Some(Harness::Omp),
                since: 0,
            };
            std::fs::write(error_path(&dir.0, "w"), record).unwrap();
            assert_eq!(
                decide(3, Ended::Watched, pane, false, &w),
                hit(ProviderError::Retryable("5xx")),
                "{record}"
            );
        }
    }

    /// A record the writer calls fatal is never retried, even when its status
    /// is not one the table knows as fatal.
    #[test]
    fn contradictory_error_record_fails_closed() {
        let dir = TempDir::new("contradiction");
        let w = Worker {
            name: "w",
            runs_dir: &dir.0,
            harness: Some(Harness::Omp),
            since: 0,
        };
        std::fs::write(error_path(&dir.0, "w"), "fatal 429 0\n").unwrap();
        assert_eq!(
            decide(3, Ended::Watched, "", false, &w),
            hit(ProviderError::Fatal("harness-fatal"))
        );
        std::fs::write(error_path(&dir.0, "w"), "retryable 401 0\n").unwrap();
        assert_eq!(
            decide(3, Ended::Watched, "", false, &w),
            hit(ProviderError::Fatal("auth-or-quota"))
        );
    }

    /// #196 review: a record carries a status but no reason word, so its
    /// 401/403 must not claim "signed-out" when it may be an exhausted quota.
    #[test]
    fn record_auth_status_gets_a_neutral_label() {
        let dir = TempDir::new("auth-label");
        let w = Worker {
            name: "w",
            runs_dir: &dir.0,
            harness: Some(Harness::Omp),
            since: 0,
        };
        for (record, want) in [
            ("fatal 403 0", "auth-or-quota"),
            ("fatal 401 0", "auth-or-quota"),
            ("fatal 402 0", "billing"),
        ] {
            std::fs::write(error_path(&dir.0, "w"), record).unwrap();
            assert_eq!(
                decide(3, Ended::Watched, "", false, &w),
                hit(ProviderError::Fatal(want)),
                "{record}"
            );
        }
    }

    /// #196 review: a record written before the watched turn started belongs
    /// to an earlier turn — the harness failed that one before quaqua began
    /// watching — so the pane decides, not the stale verdict.
    #[test]
    fn a_record_from_before_the_turn_start_is_ignored() {
        let dir = TempDir::new("stale-record");
        let path = error_path(&dir.0, "w");
        let now = now_sec();
        let w = Worker {
            name: "w",
            runs_dir: &dir.0,
            harness: Some(Harness::Omp),
            since: now.saturating_sub(5),
        };
        std::fs::write(&path, "fatal 401 0\n").unwrap();
        assert_eq!(
            decide(3, Ended::Watched, "", false, &w),
            hit(ProviderError::Fatal("auth-or-quota")),
            "a record written during this turn is trusted"
        );
        std::fs::write(&path, "fatal 401 0\n").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let pane = "Error: 503 Service Unavailable";
        assert_eq!(
            decide(3, Ended::Watched, pane, false, &w),
            hit(ProviderError::Retryable("5xx")),
            "the previous turn's verdict must not escalate this stall"
        );
        assert!(
            !path.exists(),
            "the stale record is dropped, not left behind"
        );
    }

    /// #196 review: opencode has no fixture of its own, so it keeps every
    /// shape — including the Claude tables it had before #196.
    #[test]
    fn classify_opencode_keeps_every_shape() {
        for (text, want) in [
            (
                "API Error: 529 Overloaded",
                ProviderError::Retryable("overloaded"),
            ),
            (
                "Error: 503 Service Unavailable",
                ProviderError::Retryable("5xx"),
            ),
            (
                "529: {\"type\":\"overloaded_error\"}",
                ProviderError::Retryable("overloaded"),
            ),
            (
                "Not logged in · Please run /login",
                ProviderError::Fatal("signed-out"),
            ),
        ] {
            assert_eq!(classify(Some(Harness::Opencode), text), hit(want), "{text}");
        }
    }

    #[test]
    fn decide_skips_cost_cap_unlanded_and_pre_nudge_text() {
        let dir = TempDir::new("decide");
        let w = Worker {
            name: "w",
            runs_dir: &dir.0,
            harness: Some(Harness::Claude),
            since: 0,
        };
        let pane = "API Error: 529 Overloaded";
        let overloaded = hit(ProviderError::Retryable("overloaded"));
        assert_eq!(decide(5, Ended::Watched, pane, false, &w), overloaded);
        assert_eq!(decide(3, Ended::Watched, pane, false, &w), overloaded);
        assert_eq!(
            decide(5, Ended::CostCap, pane, false, &w),
            None,
            "rev177 F2"
        );
        assert_eq!(
            decide(5, Ended::NotLanded, pane, false, &w),
            None,
            "rev177 F4"
        );
        assert_eq!(
            decide(3, Ended::NotLanded, pane, false, &w),
            None,
            "rev177 F4"
        );
        assert_eq!(decide(4, Ended::Watched, pane, false, &w), None);
        // rev177 F3: after a "continue", the old error is history.
        let after = "API Error: 529 Overloaded\n> continue\n⏺ Working\nDo you want to proceed?";
        assert_eq!(decide(5, Ended::Watched, after, true, &w), None);
        let again = "API Error: 529 Overloaded\n> continue\nAPI Error: 529 Overloaded";
        assert_eq!(decide(5, Ended::Watched, again, true, &w), overloaded);
        // pi and omp panes classify through the same decide.
        let pi = Worker {
            harness: Some(Harness::Pi),
            since: 0,
            ..w
        };
        assert_eq!(
            decide(3, Ended::Watched, "Error: 429: {}", false, &pi),
            hit(ProviderError::Retryable("rate-limit"))
        );
    }
}
