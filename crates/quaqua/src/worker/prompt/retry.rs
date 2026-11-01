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

const FATAL: &[(&str, &str)] = &[
    ("signed out", "signed-out"),
    ("not logged in", "signed-out"),
    ("please log in", "signed-out"),
    ("please run /login", "signed-out"),
    ("auth_required", "signed-out"),
    ("invalid api key", "signed-out"),
    ("authentication_error", "signed-out"),
    ("insufficient_quota", "quota"),
    ("quota exceeded", "quota"),
    ("exceeded your current quota", "quota"),
    ("usage limit reached", "quota"),
    ("credit balance is too low", "billing"),
    ("insufficient credits", "billing"),
    ("billing", "billing"),
];

const RETRYABLE: &[(&str, &str)] = &[
    ("overloaded", "overloaded"),
    ("rate limit", "rate-limit"),
    ("rate_limit", "rate-limit"),
    ("too many requests", "rate-limit"),
    ("internal server error", "5xx"),
    ("bad gateway", "5xx"),
    ("service unavailable", "5xx"),
    ("gateway timeout", "5xx"),
    ("api error: 5", "5xx"),
    ("econnreset", "network"),
    ("connection reset", "network"),
    ("connection error", "network"),
    ("network error", "network"),
    ("fetch failed", "network"),
    ("socket hang up", "network"),
    ("etimedout", "network"),
];

/// Classify pane text. Fatal wins over retryable: a signed-out pane that
/// also shows an earlier overload must not be retried.
pub fn classify(pane: &str) -> Option<ProviderError> {
    let text = pane.to_lowercase();
    let hit = |table: &[(&str, &'static str)]| {
        table
            .iter()
            .find(|(pat, _)| text.contains(pat))
            .map(|(_, r)| *r)
    };
    hit(FATAL)
        .map(ProviderError::Fatal)
        .or_else(|| hit(RETRYABLE).map(ProviderError::Retryable))
}

pub const TAIL_LINES: usize = 15;

/// How a `prompt_worker_once` run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Watched,
    CostCap,
    NotLanded,
}

pub fn decide(code: i32, _ended: Ended, pane: &str, _nudged: bool) -> Option<ProviderError> {
    if !matches!(code, 3 | 5) {
        return None;
    }
    classify(pane)
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
