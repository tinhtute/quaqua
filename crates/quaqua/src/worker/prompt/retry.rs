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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinePrefix {
    RetryExhausted, // omp "Retry budget exhausted after N retries: "
    RetryFailed,    // pi "Retry failed after N attempts: "
    ErrorPrefix,    // "Error: "
    None,           // bare line
}

fn parse_prefix(s: &str) -> (&str, LinePrefix) {
    let mut cur = s;
    let mut has_error = false;
    if cur.len() >= 6 && cur[..6].eq_ignore_ascii_case("error:") {
        cur = cur[6..].trim_start();
        has_error = true;
    }

    if cur.len() >= 29 && cur[..29].eq_ignore_ascii_case("retry budget exhausted after ") {
        let rest = &cur[29..];
        if let Some(pos) = rest.find(':') {
            let mid = &rest[..pos];
            if mid.ends_with("retries")
                && mid[..mid.len() - 7]
                    .trim()
                    .chars()
                    .all(|c| c.is_ascii_digit())
            {
                return (rest[pos + 1..].trim_start(), LinePrefix::RetryExhausted);
            }
        }
    }

    if cur.len() >= 19 && cur[..19].eq_ignore_ascii_case("retry failed after ") {
        let rest = &cur[19..];
        if let Some(pos) = rest.find(':') {
            let mid = &rest[..pos];
            if mid.ends_with("attempts")
                && mid[..mid.len() - 8]
                    .trim()
                    .chars()
                    .all(|c| c.is_ascii_digit())
            {
                return (rest[pos + 1..].trim_start(), LinePrefix::RetryFailed);
            }
        }
    }

    if has_error {
        (cur, LinePrefix::ErrorPrefix)
    } else {
        (s, LinePrefix::None)
    }
}

fn classify_status(status: u16, full_text: &str) -> Option<ProviderError> {
    let lower = full_text.to_lowercase();
    match status {
        401 | 403 => {
            if lower.contains("insufficient_quota") || lower.contains("exceeded your current quota")
            {
                Some(ProviderError::Fatal("quota"))
            } else if lower.contains("invalid_api_key")
                || lower.contains("invalid api key")
                || lower.contains("incorrect api key")
                || lower.contains("credential")
            {
                Some(ProviderError::Fatal("signed-out"))
            } else {
                None
            }
        }
        408 => Some(ProviderError::Retryable("timeout")),
        409 => Some(ProviderError::Retryable("conflict")),
        425 => Some(ProviderError::Retryable("too-early")),
        429 => Some(ProviderError::Retryable("rate-limit")),
        529 => Some(ProviderError::Retryable("overloaded")),
        500..=599 => Some(ProviderError::Retryable("5xx")),
        _ => None,
    }
}

/// One pane line → its provider error, if the line has the harness shape.
fn classify_line(line: &str) -> Option<ProviderError> {
    let trimmed =
        line.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '⎿' | '⏺' | '●'));
    let l = trimmed.to_lowercase();
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
    if let Some((_, r)) = FATAL_LINES
        .iter()
        .find(|(p, _)| l.starts_with(p) || (*p == "please run /login" && l.contains(p)))
    {
        return Some(ProviderError::Fatal(r));
    }

    // Connection refused (omp network drop: "Error: Couldn't connect to ... (connection refused)...")
    if (l.starts_with("error: couldn't connect") || l.starts_with("couldn't connect"))
        && l.contains("connection refused")
    {
        return Some(ProviderError::Retryable("network"));
    }

    // pi label format: "<Label> API error (<status>): ..."
    if let Some(idx) = l.find("api error (") {
        let after = &l[idx + "api error (".len()..];
        if let Some(end_paren) = after.find(')') {
            let status_str = &after[..end_paren];
            if let Ok(status) = status_str.parse::<u16>() {
                if let Some(err) = classify_status(status, trimmed) {
                    return Some(err);
                }
            }
        }
    }

    // Status code patterns (omp and pi):
    // omp: ^\s*(Error: )?(Retry budget exhausted after \d+ retries: )?(\d{3})
    // pi:  ^\s*(Error: )?(Retry failed after \d+ attempts: )?(\d{3}):
    let (after_prefix, prefix) = parse_prefix(trimmed);
    let bytes = after_prefix.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2].is_ascii_digit()
    {
        if bytes.len() == 3 || !bytes[3].is_ascii_digit() {
            let status = (bytes[0] - b'0') as u16 * 100
                + (bytes[1] - b'0') as u16 * 10
                + (bytes[2] - b'0') as u16;
            let rest = &after_prefix[3..];

            // pi style: status followed by colon, e.g. "429: {...}"
            if let Some(body) = rest.strip_prefix(':') {
                let body = body.trim_start();
                if prefix != LinePrefix::None || body.starts_with('{') {
                    if let Some(err) = classify_status(status, trimmed) {
                        return Some(err);
                    }
                } else if matches!(status, 401 | 403) {
                    if let Some(err) = classify_status(status, trimmed) {
                        return Some(err);
                    }
                }
            }
            // omp style: status followed by space, e.g. "429 Rate limit reached..."
            else if rest.starts_with(' ') || rest.starts_with('\t') {
                if prefix != LinePrefix::None {
                    if let Some(err) = classify_status(status, trimmed) {
                        return Some(err);
                    }
                } else if matches!(status, 401 | 403) {
                    // Bare 401/403 only classifies with explicit quota/credential/key markers
                    if let Some(err) = classify_status(status, trimmed) {
                        return Some(err);
                    }
                }
            }
        }
    }

    None
}

/// Classify the last `TAIL_LINES` non-empty pane lines. Fatal wins over
/// retryable: a signed-out pane that also shows an earlier overload must not
/// be retried.
pub fn classify(pane: &str) -> Option<ProviderError> {
    let lines: Vec<&str> = pane.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(TAIL_LINES)..];
    let mut hits = Vec::new();
    for (i, line) in tail.iter().enumerate() {
        if let Some(err) = classify_line(line) {
            hits.push(err);
        } else {
            // Check if line opened a bare 401/403 whose markers were wrapped into continuation lines
            let trimmed = line
                .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '⎿' | '⏺' | '●'));
            let (after_prefix, _) = parse_prefix(trimmed);
            let bytes = after_prefix.as_bytes();
            if bytes.len() >= 3
                && bytes[0].is_ascii_digit()
                && bytes[1].is_ascii_digit()
                && bytes[2].is_ascii_digit()
            {
                if bytes.len() == 3 || !bytes[3].is_ascii_digit() {
                    let status = (bytes[0] - b'0') as u16 * 100
                        + (bytes[1] - b'0') as u16 * 10
                        + (bytes[2] - b'0') as u16;
                    if matches!(status, 401 | 403) {
                        let mut combined = String::from(*line);
                        for next in &tail[i + 1..tail.len().min(i + 4)] {
                            if next.starts_with(' ') || next.starts_with('\t') {
                                combined.push(' ');
                                combined.push_str(next.trim());
                            } else {
                                break;
                            }
                        }
                        if let Some(err) = classify_status(status, &combined) {
                            hits.push(err);
                        }
                    }
                }
            }
        }
    }
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

    /// #196: omp provider-error text fixtures (transient and fatal).
    #[test]
    fn classify_omp_provider_errors() {
        for (text, want) in [
            (
                "Error: 429 Rate limit reached for gpt-5.1-codex-mini on requests per min (RPM): ... (type=requests param=rate_limit_exceeded)",
                ProviderError::Retryable("rate-limit"),
            ),
            (
                "Retry budget exhausted after 10 retries: 429 Rate limit reached ...",
                ProviderError::Retryable("rate-limit"),
            ),
            (
                "Error: Couldn't connect to 127.0.0.1:8899 (connection refused). Check that the server is running and reachable.",
                ProviderError::Retryable("network"),
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
        ] {
            assert_eq!(classify(text), Some(want), "{text}");
        }
    }

    /// #196: pi provider-error text fixtures (transient and fatal).
    #[test]
    fn classify_pi_provider_errors() {
        for (text, want) in [
            (
                r#"Error: 429: {"message":"Rate limit reached ...","code":"rate_limit_exceeded"}"#,
                ProviderError::Retryable("rate-limit"),
            ),
            (
                r#"Error: Retry failed after 3 attempts: 429: {...}"#,
                ProviderError::Retryable("rate-limit"),
            ),
            (
                r#"OpenAI API error (429): {...}"#,
                ProviderError::Retryable("rate-limit"),
            ),
            (
                r#"401: {"message":"Incorrect API key provided: ...","code":"invalid_api_key"}"#,
                ProviderError::Fatal("signed-out"),
            ),
            (
                r#"403: {"message":"You exceeded your current quota, ...","code":"insufficient_quota"}"#,
                ProviderError::Fatal("quota"),
            ),
            (
                r#"529: {"type":"overloaded_error","message":"Overloaded"}"#,
                ProviderError::Retryable("overloaded"),
            ),
        ] {
            assert_eq!(classify(text), Some(want), "{text}");
        }
    }

    /// #196: multi-line wrapped TUI banners in omp and pi.
    #[test]
    fn classify_wrapped_tui_banners() {
        let omp_wrapped_429 = "\
 Error: 429 Rate limit reached for gpt-5.1-codex-mini on requests per min
   (RPM): Limit 500, Used 500, Requested 1. Please try again in 20ms.
   Rate limit reached for gpt-5.1-codex-mini on requests per min (RPM): Limit
   500, Used 500, Requested 1. Please try again in 20ms. (type=requests
   param=rate_limit_exceeded)";
        assert_eq!(
            classify(omp_wrapped_429),
            Some(ProviderError::Retryable("rate-limit"))
        );

        let omp_wrapped_conn_refused = "\
 Error: Couldn't connect to 127.0.0.1:8899 (connection refused). Check that
   the server is running and reachable.";
        assert_eq!(
            classify(omp_wrapped_conn_refused),
            Some(ProviderError::Retryable("network"))
        );

        let pi_wrapped_429 = "\
 Error: 429: {\"message\":\"Rate limit reached for gpt-5.1-codex-mini on requests
 per min (RPM): Limit 500, Used 500, Requested 1. Please try again in
 20ms.\",\"type\":\"requests\",\"param\":null,\"code\":\"rate_limit_exceeded\"}";
        assert_eq!(
            classify(pi_wrapped_429),
            Some(ProviderError::Retryable("rate-limit"))
        );

        let pi_wrapped_terminal = "\
 Error: Retry failed after 3 attempts: 429: {\"message\":\"Rate limit reached for
 gpt-5.1-codex-mini on requests per min (RPM): Limit 500, Used 500, Requested
 1. Please try again in
 20ms.\",\"type\":\"requests\",\"param\":null,\"code\":\"rate_limit_exceeded\"}";
        assert_eq!(
            classify(pi_wrapped_terminal),
            Some(ProviderError::Retryable("rate-limit"))
        );
    }

    /// Status codes: 408/409/425/5xx.
    #[test]
    fn classify_status_codes_retryable() {
        for (text, want) in [
            (
                "Error: 408 Request Timeout",
                ProviderError::Retryable("timeout"),
            ),
            (
                r#"408: {"message":"Request Timeout"}"#,
                ProviderError::Retryable("timeout"),
            ),
            ("Error: 409 Conflict", ProviderError::Retryable("conflict")),
            (
                r#"409: {"message":"Conflict"}"#,
                ProviderError::Retryable("conflict"),
            ),
            (
                "Error: 425 Too Early",
                ProviderError::Retryable("too-early"),
            ),
            (
                r#"425: {"message":"Too Early"}"#,
                ProviderError::Retryable("too-early"),
            ),
            (
                "Error: 500 Internal Server Error",
                ProviderError::Retryable("5xx"),
            ),
            (
                r#"500: {"message":"Internal Server Error"}"#,
                ProviderError::Retryable("5xx"),
            ),
            (
                "Error: 503 Service Unavailable",
                ProviderError::Retryable("5xx"),
            ),
            (
                r#"503: {"message":"Service Unavailable"}"#,
                ProviderError::Retryable("5xx"),
            ),
        ] {
            assert_eq!(classify(text), Some(want), "{text}");
        }
    }

    /// Free text that mentions status numbers or looks like compiler/tool output
    /// must not classify as provider errors.
    #[test]
    fn classify_ignores_free_text_that_mentions_status_numbers() {
        for pane in [
            "Allow Bash(cargo test -p status429 -- 429 items)?\n1. Yes 2. No",
            "HTTP 500 error handling added to api.rs",
            "Listening on port 5290",
            "429 tests passed, 0 failed",
            "401 items in catalog",
            "403 forbidden files skipped",
            "500 lines compiled successfully",
            "warning: error 429 occurred during simulation",
            "line 429: let x = 1;",
            "429: let x = 1;",
            "408 requests processed",
        ] {
            assert_eq!(classify(pane), None, "{pane}");
        }
    }
}
