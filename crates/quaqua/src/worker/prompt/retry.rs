//! Provider-error retry (#177): classify a turn that ended without a report
//! from the agent's pane text, retry a transient provider error with capped
//! exponential backoff, and escalate the rest.
//!
//! Knobs: `PROMPT_WORKER_RETRY_BASE_MS` (first delay, default 60000 = 1 min;
//! delays double: 1, 2, 4, 8 min) and `PROMPT_WORKER_RETRY_MAX` (default 4).

use crate::engine::journal::{EventRecord, Journal};
use crate::engine::journal_sqlite::SqliteJournal;
use crate::util::now_sec;
use std::path::Path;

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

/// Lines that open a harness provider-error message (lowercased, after the
/// `⎿`/`⏺`/`●` gutter). Claude Code prints `API Error: …`, a bare
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

/// One pane line → its provider error, if the line has the harness shape.
fn classify_line(line: &str) -> Option<ProviderError> {
    let l = line
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '⎿' | '⏺' | '●'))
        .to_lowercase();
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

/// Classify the last `TAIL_LINES` non-empty pane lines. Fatal wins over
/// retryable: a signed-out pane that also shows an earlier overload must not
/// be retried.
pub fn classify(pane: &str) -> Option<ProviderError> {
    let lines: Vec<&str> = pane.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(TAIL_LINES)..];
    let hits: Vec<ProviderError> = tail.iter().filter_map(|l| classify_line(l)).collect();
    hits.iter()
        .find(|e| matches!(e, ProviderError::Fatal(_)))
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
/// kill or a brief that never landed. After a `continue` nudge (`nudged`),
/// only the pane text below the echoed nudge counts.
pub fn decide(code: i32, ended: Ended, pane: &str, nudged: bool) -> Option<ProviderError> {
    if !matches!(code, 3 | 5) || ended != Ended::Watched {
        return None;
    }
    let text = if nudged { after_nudge(pane) } else { pane };
    classify(text)
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

    #[test]
    fn classify_separates_transient_from_human_errors() {
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
            assert_eq!(classify(text), Some(want), "{text}");
        }
        assert_eq!(classify("I finished the task and wrote the report."), None);
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
        ] {
            assert_eq!(classify(pane), None, "{pane}");
        }
    }

    /// Only the tail of the pane counts: an error far above the last lines
    /// is history, not the reason the turn stopped.
    #[test]
    fn classify_reads_only_the_last_lines() {
        let mut pane = String::from("API Error: 529 Overloaded\n");
        for i in 0..TAIL_LINES {
            pane.push_str(&format!("line {i}\n"));
        }
        assert_eq!(classify(&pane), None);
    }

    #[test]
    fn decide_skips_cost_cap_unlanded_and_pre_nudge_text() {
        let pane = "API Error: 529 Overloaded";
        let hit = Some(ProviderError::Retryable("overloaded"));
        assert_eq!(decide(5, Ended::Watched, pane, false), hit);
        assert_eq!(decide(3, Ended::Watched, pane, false), hit);
        assert_eq!(decide(5, Ended::CostCap, pane, false), None, "rev177 F2");
        assert_eq!(decide(5, Ended::NotLanded, pane, false), None, "rev177 F4");
        assert_eq!(decide(3, Ended::NotLanded, pane, false), None, "rev177 F4");
        assert_eq!(decide(4, Ended::Watched, pane, false), None);
        // rev177 F3: after a "continue", the old error is history.
        let after = "API Error: 529 Overloaded\n> continue\n⏺ Working\nDo you want to proceed?";
        assert_eq!(decide(5, Ended::Watched, after, true), None);
        let again = "API Error: 529 Overloaded\n> continue\nAPI Error: 529 Overloaded";
        assert_eq!(decide(5, Ended::Watched, again, true), hit);
    }
}
