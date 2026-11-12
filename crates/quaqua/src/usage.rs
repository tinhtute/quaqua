//! OpenCode usage audit — text/JSON report over request logs, meter, local DB and runs.

use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const CONSOLE_BASE: &str = "https://opencode.ai/console";
const DB_DEFAULT: &str = "~/.local/share/opencode/opencode.db";
const REPO_DEFAULT: &str = "~/quaqua";
const PAGE_LIMIT: usize = 100;
const VN_OFFSET_MS: i64 = 7 * 3600 * 1000;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "usage",
    about = "OpenCode usage audit (text-only) — task cost = whole session (matches watchdog), day rows = window"
)]
pub struct UsageArgs {
    #[arg(long, default_value_t = 7, help = "number of VN calendar days (default 7)")]
    pub days: u32,

    #[arg(long, help = "emit JSON instead of text")]
    pub json: bool,

    #[arg(long, help = "anchor unix ms (default: now)")]
    pub until: Option<i64>,

    #[arg(long, help = "offline fixture dir (no network)")]
    pub fixture_dir: Option<PathBuf>,

    #[arg(long, help = "override local opencode version (offline)")]
    pub local_version: Option<String>,

    #[arg(long, help = "path to opencode.db (default ~/.local/share/opencode/opencode.db)")]
    pub db: Option<PathBuf>,

    #[arg(long, help = "repo for git outcomes (default ~/quaqua)")]
    pub repo: Option<PathBuf>,

    #[arg(long, help = "runs dir (default <repo>/runs)")]
    pub runs_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.reasoning += other.reasoning;
        self.cache_read += other.cache_read;
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Totals {
    pub requests: u64,
    pub cost: f64,
    pub tokens: Tokens,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MachineStat {
    pub ua: String,
    pub version: Option<String>,
    pub requests: u64,
    pub cost: f64,
    pub local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DayStat {
    pub date: String,
    pub requests: u64,
    pub cost: f64,
    pub tokens: Tokens,
    pub first: Option<i64>,
    pub last: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskStat {
    pub task: String,
    pub spend_src: String,
    pub sessions: Vec<String>,
    pub requests: u64,
    pub cost: f64,
    pub tokens: Tokens,
    pub wall_min: u64,
    pub idle_min: u64,
    pub outcome: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SplitCategory {
    pub calls_pct: f64,
    pub cost_pct: f64,
    pub calls: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WasteRespawn {
    pub task: String,
    pub replaced_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WasteKilled {
    pub task: String,
    pub idle_outcome: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WasteIdle {
    pub task: String,
    pub idle_min: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WasteReview {
    pub task: String,
    pub rounds: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Waste {
    pub respawn: Vec<WasteRespawn>,
    pub killed: Vec<WasteKilled>,
    pub idle_gt_30m: Vec<WasteIdle>,
    pub review_rounds: Vec<WasteReview>,
}

impl Waste {
    pub fn is_empty(&self) -> bool {
        self.respawn.is_empty()
            && self.killed.is_empty()
            && self.idle_gt_30m.is_empty()
            && self.review_rounds.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MeterWindow {
    pub percent: Option<i64>,
    pub status: Option<String>,
    pub resets_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_complete: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageReport {
    pub generated: String,
    pub offline: bool,
    pub days: u32,
    pub until: String,
    pub cutoff: String,
    pub local_version: Option<String>,
    pub totals: Totals,
    pub warn_other_machine: bool,
    pub machines: Vec<MachineStat>,
    pub days_table: Vec<DayStat>,
    pub tasks: Vec<TaskStat>,
    pub split: BTreeMap<String, SplitCategory>,
    pub waste: Waste,
    pub meter: BTreeMap<String, MeterWindow>,
    #[serde(rename = "meterUnavailable")]
    pub meter_unavailable: Option<String>,
    #[serde(rename = "requestLogsError")]
    pub request_logs_error: Option<String>,
    #[serde(rename = "requestLogsPartial")]
    pub request_logs_partial: Option<String>,
    pub notes: Vec<String>,
    pub exit_code: i32,
}

// ---------------------------------------------------------------- date & civil math

pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let days = days + 719468;
    let era = (if days >= 0 { days } else { days - 146096 }) / 146097;
    let doe = (days - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y as i32, m, d)
}

pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era as i64 * 146097 + doe as i64 - 719468
}

pub fn format_iso_vn(ms: i64) -> String {
    let ms_vn = ms + VN_OFFSET_MS;
    let days = ms_vn.div_euclid(86_400_000);
    let rem_ms = ms_vn.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let sec = rem_ms / 1000;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+07:00",
        y,
        m,
        d,
        sec / 3600,
        (sec / 60) % 60,
        sec % 60
    )
}

pub fn day_key(ms: i64) -> String {
    let ms_vn = ms + VN_OFFSET_MS;
    let days = ms_vn.div_euclid(86_400_000);
    let (_, m, d) = civil_from_days(days);
    format!("{:02}/{:02}", d, m)
}

pub fn parse_iso_utc(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s.strip_suffix('Z').unwrap_or(s);
    let s = s.strip_suffix("+00:00").unwrap_or(s);
    let (date_part, time_part) = s.split_once('T')?;
    let mut date_segs = date_part.split('-');
    let y: i32 = date_segs.next()?.parse().ok()?;
    let m: u32 = date_segs.next()?.parse().ok()?;
    let d: u32 = date_segs.next()?.parse().ok()?;

    let (time_main, frac) = time_part.split_once('.').unwrap_or((time_part, "0"));
    let mut time_segs = time_main.split(':');
    let h: i64 = time_segs.next()?.parse().ok()?;
    let min: i64 = time_segs.next()?.parse().ok()?;
    let sec: i64 = time_segs.next()?.parse().ok()?;

    let mut millis = 0i64;
    if !frac.is_empty() {
        let frac_3 = if frac.len() >= 3 {
            &frac[..3]
        } else {
            frac
        };
        if let Ok(v) = frac_3.parse::<i64>() {
            millis = match frac_3.len() {
                1 => v * 100,
                2 => v * 10,
                _ => v,
            };
        }
    }

    let days = days_from_civil(y, m, d);
    let total_ms = days * 86_400_000 + (h * 3600 + min * 60 + sec) * 1000 + millis;
    Some(total_ms)
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn expand_path(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

// ---------------------------------------------------------------- tripwire & kfmt

pub fn contains_token(text: &str) -> bool {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i + 3 < n {
        if (bytes[i] == b's' || bytes[i] == b'r') && bytes[i + 1] == b't' && bytes[i + 2] == b'_' {
            let mut j = i + 3;
            while j < n
                && (bytes[j].is_ascii_alphanumeric()
                    || bytes[j] == b'.'
                    || bytes[j] == b'_'
                    || bytes[j] == b'-')
            {
                j += 1;
            }
            if j - (i + 3) >= 10 {
                return true;
            }
        }
        i += 1;
    }
    false
}

pub fn kfmt(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", (n as f64 / 1_000.0).round())
    } else {
        n.to_string()
    }
}

// ---------------------------------------------------------------- sources

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceUnavailable {
    pub source: String,
    pub reason: String,
}

impl std::fmt::Display for SourceUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} unavailable: {}", self.source, self.reason)
    }
}

pub fn local_version() -> Option<String> {
    let out = Command::new("opencode").arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    extract_version(&s)
}

fn extract_version(s: &str) -> Option<String> {
    for mut part in s.split(|c: char| c.is_whitespace() || c == '/' || c == '(' || c == ')') {
        part = part.strip_prefix('v').unwrap_or(part);
        let bytes = part.as_bytes();
        if bytes.is_empty() {
            continue;
        }
        let dot_count = bytes.iter().filter(|&&b| b == b'.').count();
        if dot_count >= 2 && bytes[0].is_ascii_digit() {
            let valid = bytes
                .iter()
                .all(|&b| b.is_ascii_digit() || b == b'.' || b.is_ascii_alphabetic() || b == b'-');
            if valid {
                return Some(part.to_string());
            }
        }
    }
    None
}

pub fn get_token() -> Result<String, String> {
    if let Ok(tok) = std::env::var("OPENCODE_TOKEN") {
        if !tok.trim().is_empty() {
            return Ok(tok.trim().to_string());
        }
    }
    if let Ok(tok) = std::env::var("OPENCODE_ACCESS_TOKEN") {
        if !tok.trim().is_empty() {
            return Ok(tok.trim().to_string());
        }
    }
    let out = Command::new("opencode")
        .args(["auth", "export", "opencode"])
        .output()
        .map_err(|e| format!("failed to run opencode auth export: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "opencode auth export exited with status {}",
            out.status
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let entries: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .map_err(|e| format!("bad JSON from opencode auth export: {e}"))?;
    let mut best: Option<String> = None;
    for e in entries {
        if let Some(val) = e.get("value") {
            if let Some(access) = val.get("access").and_then(|a| a.as_str()) {
                let is_active = e.get("active").and_then(|a| a.as_bool()).unwrap_or(false);
                if is_active || best.is_none() {
                    best = Some(access.to_string());
                }
            }
        }
    }
    best.ok_or_else(|| "no access token found in `opencode auth export opencode`".to_string())
}

pub fn http_json_with_sleep<F>(
    url: &str,
    token: &str,
    ua: &str,
    attempts: usize,
    mut sleep_fn: F,
) -> Result<serde_json::Value, SourceUnavailable>
where
    F: FnMut(u64),
{
    let mut last = "unknown error".to_string();
    for i in 0..attempts {
        let resp = ureq::get(url)
            .set("Authorization", &format!("Bearer {}", token))
            .set("User-Agent", ua)
            .timeout(std::time::Duration::from_secs(60))
            .call();

        match resp {
            Ok(r) => {
                let status = r.status();
                if status < 500 && status != 429 && status >= 400 {
                    return Err(SourceUnavailable {
                        source: url.to_string(),
                        reason: format!("HTTP {}", status),
                    });
                }
                match r.into_string() {
                    Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
                        Ok(val) => return Ok(val),
                        Err(_) => {
                            last = "bad JSON".to_string();
                        }
                    },
                    Err(e) => {
                        last = format!("network error: {}", e);
                    }
                }
            }
            Err(ureq::Error::Status(code, _resp)) => {
                if code < 500 && code != 429 {
                    return Err(SourceUnavailable {
                        source: url.to_string(),
                        reason: format!("HTTP {}", code),
                    });
                }
                last = format!("HTTP {}", code);
            }
            Err(ureq::Error::Transport(t)) => {
                last = format!("network error: {}", t);
            }
        }
        if i + 1 < attempts {
            let s = std::cmp::min(1u64 << i, 4);
            sleep_fn(s);
        }
    }
    Err(SourceUnavailable {
        source: url.to_string(),
        reason: last,
    })
}

pub fn http_json(
    url: &str,
    token: &str,
    ua: &str,
    attempts: usize,
) -> Result<serde_json::Value, SourceUnavailable> {
    http_json_with_sleep(url, token, ua, attempts, |sec| {
        std::thread::sleep(std::time::Duration::from_secs(sec));
    })
}

pub fn fetch_request_logs(
    token: &str,
    ua: &str,
    cutoff_ms: i64,
    until_ms: i64,
) -> Result<(Vec<serde_json::Value>, Option<String>), SourceUnavailable> {
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;

    for pages in 0..200 {
        let mut url = format!(
            "{}/api/request-logs?category=inference&limit={}&until={}",
            CONSOLE_BASE, PAGE_LIMIT, until_ms
        );
        if let Some(c) = &cursor {
            url.push_str("&cursor=");
            url.push_str(c);
        }
        let d = match http_json(&url, token, ua, 10) {
            Ok(v) => v,
            Err(e) => {
                if rows.is_empty() {
                    return Err(e);
                }
                return Ok((
                    dedupe_rows(rows),
                    Some(format!("stopped after {} page(s): {}", pages, e.reason)),
                ));
            }
        };
        let items = d.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let oldest = items.iter().filter_map(|i| i.get("startedAt").and_then(|v| v.as_i64())).min();
        let next_cursor = d.get("nextCursor").and_then(|v| v.as_str()).map(|s| s.to_string());
        let has_items = !items.is_empty();
        rows.extend(items);
        cursor = next_cursor;

        if !has_items || cursor.is_none() || oldest.is_none() || oldest.unwrap() < cutoff_ms {
            break;
        }
    }
    Ok((dedupe_rows(rows), None))
}

fn dedupe_rows(rows: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for r in rows {
        let id = r.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
        if let Some(id_str) = id {
            if seen.insert(id_str) {
                out.push(r);
            }
        } else {
            out.push(r);
        }
    }
    out
}

pub fn fetch_meter(token: &str, ua: &str) -> Result<serde_json::Value, SourceUnavailable> {
    http_json(
        &format!("{}/inference/go/v1/usage", CONSOLE_BASE),
        token,
        ua,
        10,
    )
}

pub fn fixture_logs(dirpath: &Path) -> Result<Vec<serde_json::Value>, SourceUnavailable> {
    let path = dirpath.join("request_logs.json");
    let content = std::fs::read_to_string(&path).map_err(|_| SourceUnavailable {
        source: "request-logs fixture".to_string(),
        reason: format!("missing {}", path.display()),
    })?;
    let val: serde_json::Value =
        serde_json::from_str(&content).map_err(|_| SourceUnavailable {
            source: "request-logs fixture".to_string(),
            reason: format!("bad JSON in {}", path.display()),
        })?;
    let mut rows = Vec::new();
    if let Some(pages) = val.get("pages").and_then(|v| v.as_array()) {
        for page in pages {
            if let Some(items) = page.get("items").and_then(|v| v.as_array()) {
                rows.extend(items.clone());
            }
        }
    }
    Ok(rows)
}

pub fn fixture_meter(dirpath: &Path) -> Result<serde_json::Value, SourceUnavailable> {
    let path = dirpath.join("meter.json");
    let content = std::fs::read_to_string(&path).map_err(|_| SourceUnavailable {
        source: "meter fixture".to_string(),
        reason: format!("missing {}", path.display()),
    })?;
    serde_json::from_str(&content).map_err(|_| SourceUnavailable {
        source: "meter fixture".to_string(),
        reason: format!("bad JSON in {}", path.display()),
    })
}

// ---------------------------------------------------------------- tool classification

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

pub fn is_process_path(target: &str) -> bool {
    if target.contains("brief") {
        return true;
    }
    if target.ends_with("team.toml")
        || target.ends_with(".brief.md")
        || target.ends_with(".plan.md")
        || target.ends_with("AGENTS.md")
        || target.ends_with("SOUL.md")
        || target.ends_with("USER.md")
        || target.ends_with("identity.md")
        || target.ends_with("models.md")
    {
        return true;
    }
    let names = &["runs", "rules", "memory", "raw", "reports", "docs/plans"];
    let n = target.len();
    for &name in names {
        let mut pos = 0;
        while let Some(idx) = target[pos..].find(name) {
            let actual_idx = pos + idx;
            let valid_before = actual_idx == 0 || target.as_bytes()[actual_idx - 1] == b'/';
            let end_idx = actual_idx + name.len();
            let valid_after = end_idx == n || target.as_bytes()[end_idx] == b'/';
            if valid_before && valid_after {
                return true;
            }
            pos = actual_idx + 1;
        }
    }
    false
}

pub fn is_product_path(target: &str) -> bool {
    if target.ends_with("Cargo.toml")
        || target.ends_with("Cargo.lock")
        || target.ends_with(".rs")
        || target.ends_with(".py")
        || target.ends_with(".sh")
    {
        return true;
    }
    let names = &["crates/", "src/", "scripts/", "plugins/", "tests/"];
    for &name in names {
        let mut pos = 0;
        while let Some(idx) = target[pos..].find(name) {
            let actual_idx = pos + idx;
            if actual_idx == 0 || target.as_bytes()[actual_idx - 1] == b'/' {
                return true;
            }
            pos = actual_idx + 1;
        }
    }
    false
}

pub fn is_process_cmd(target: &str) -> bool {
    if target.contains("prompt-worker")
        || target.contains("spawn-worker")
        || target.contains("cleanup-worker")
    {
        return true;
    }
    let n = target.len();
    let mut pos = 0;
    while let Some(idx) = target[pos..].find("quaqua") {
        let actual_idx = pos + idx;
        if actual_idx == 0 || !is_word_char(target[..actual_idx].chars().last().unwrap()) {
            let end_idx = actual_idx + 6;
            if end_idx < n && target.as_bytes()[end_idx].is_ascii_whitespace() {
                let mut sub_start = end_idx;
                while sub_start < n && target.as_bytes()[sub_start].is_ascii_whitespace() {
                    sub_start += 1;
                }
                for &sub in &[
                    "worker", "pipeline", "lead", "config", "merge", "grid", "gate", "taskforce",
                    "advisor", "usage",
                ] {
                    if target[sub_start..].starts_with(sub) {
                        let after = sub_start + sub.len();
                        if after == n || !is_word_char(target[after..].chars().next().unwrap()) {
                            return true;
                        }
                    }
                }
            }
        }
        pos = actual_idx + 1;
    }

    pos = 0;
    while let Some(idx) = target[pos..].find("herdr") {
        let actual_idx = pos + idx;
        if actual_idx == 0 || !is_word_char(target[..actual_idx].chars().last().unwrap()) {
            let end_idx = actual_idx + 5;
            if end_idx < n && target.as_bytes()[end_idx].is_ascii_whitespace() {
                let mut sub_start = end_idx;
                while sub_start < n && target.as_bytes()[sub_start].is_ascii_whitespace() {
                    sub_start += 1;
                }
                for &sub in &["agent", "pane", "tab", "workspace"] {
                    if target[sub_start..].starts_with(sub) {
                        let after = sub_start + sub.len();
                        if after == n || !is_word_char(target[after..].chars().next().unwrap()) {
                            return true;
                        }
                    }
                }
            }
        }
        pos = actual_idx + 1;
    }
    false
}

pub fn is_product_cmd(target: &str) -> bool {
    let s = target.trim_start();
    let n = s.len();
    for &cmd in &[
        "cargo-nextest", "rustc", "pytest", "python3", "python", "npm", "node", "zkill", "cargo",
    ] {
        if s.starts_with(cmd) {
            let after = cmd.len();
            if after == n || !is_word_char(s[after..].chars().next().unwrap()) {
                return true;
            }
        }
    }
    for &prefix in &["bash", "sh"] {
        if let Some(rem) = s.strip_prefix(prefix) {
            if rem.starts_with(char::is_whitespace) {
                let rem_trim = rem.trim_start();
                if rem_trim.starts_with("tests/") {
                    return true;
                }
            }
        }
    }

    let mut pos = 0;
    while let Some(idx) = target[pos..].find("cargo") {
        let actual_idx = pos + idx;
        if actual_idx == 0 || target.as_bytes()[actual_idx - 1].is_ascii_whitespace() {
            let end_idx = actual_idx + 5;
            if end_idx < target.len() && target.as_bytes()[end_idx].is_ascii_whitespace() {
                let mut sub_start = end_idx;
                while sub_start < target.len()
                    && target.as_bytes()[sub_start].is_ascii_whitespace()
                {
                    sub_start += 1;
                }
                for &sub in &["test", "build", "check", "fmt", "clippy"] {
                    if target[sub_start..].starts_with(sub) {
                        let after = sub_start + sub.len();
                        if after == target.len()
                            || !is_word_char(target[after..].chars().next().unwrap())
                        {
                            return true;
                        }
                    }
                }
            }
        }
        pos = actual_idx + 1;
    }
    false
}

pub fn classify_tool(name: &str, inp: &serde_json::Value) -> &'static str {
    let mut target = String::new();
    if let Some(obj) = inp.as_object() {
        for key in &["command", "filePath", "file_path", "path", "pattern", "url"] {
            if let Some(s) = obj.get(*key).and_then(|v| v.as_str()) {
                target.push(' ');
                target.push_str(s);
            }
        }
    }
    let target = target.trim();

    if name == "shell" {
        if is_process_cmd(target) {
            "process"
        } else if is_product_cmd(target) {
            "product"
        } else if is_process_path(target) {
            "process"
        } else if is_product_path(target) {
            "product"
        } else {
            "other"
        }
    } else if is_process_path(target) {
        "process"
    } else if is_product_path(target) {
        "product"
    } else {
        "other"
    }
}

pub fn task_from_directory(directory: Option<&str>) -> Option<String> {
    let d = directory?.trim_end_matches('/');
    if d.is_empty() {
        return None;
    }
    if d.ends_with("/quaqua") || d == "quaqua" {
        return Some("cos".to_string());
    }
    let base = Path::new(d).file_name()?.to_string_lossy().to_string();
    if base.is_empty() {
        None
    } else {
        Some(base)
    }
}

// ---------------------------------------------------------------- outcomes & local db

#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    pub directory: Option<String>,
    pub idle_outcome: Option<String>,
}

pub fn read_sessions(db_path: &Path) -> Result<BTreeMap<String, SessionInfo>, String> {
    if !db_path.is_file() {
        return Ok(BTreeMap::new());
    }
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("local db open failed: {e}"))?;

    let has_idle = conn
        .prepare("SELECT idle_outcome FROM session_v2 LIMIT 0")
        .is_ok();
    let col_idle = if has_idle { "idle_outcome" } else { "NULL" };
    let query = format!("SELECT id, directory, {col_idle} FROM session_v2");
    let mut stmt = conn
        .prepare(&query)
        .map_err(|e| format!("db query failed: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let directory: Option<String> = row.get(1)?;
            let idle_outcome: Option<String> = row.get(2)?;
            Ok((
                id,
                SessionInfo {
                    directory,
                    idle_outcome,
                },
            ))
        })
        .map_err(|e| format!("db query failed: {e}"))?;

    let mut sessions = BTreeMap::new();
    for (id, info) in rows.flatten() {
        sessions.insert(id, info);
    }
    Ok(sessions)
}

type SessionMessages = (
    BTreeMap<String, Vec<(String, serde_json::Value)>>,
    BTreeMap<String, Vec<i64>>,
);

pub fn read_messages(
    db_path: &Path,
    session_ids: &std::collections::HashSet<String>,
) -> Result<SessionMessages, String> {
    let mut tools: BTreeMap<String, Vec<(String, serde_json::Value)>> = BTreeMap::new();
    let mut times: BTreeMap<String, Vec<i64>> = BTreeMap::new();

    let uri = format!("file:{}?mode=ro", db_path.display());
    let query = "SELECT session_id, time_created, data FROM session_message";
    let out = Command::new("sqlite3")
        .args(["-json", &uri, query])
        .output()
        .map_err(|e| format!("sqlite3 failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("sqlite3 error: {}", err.trim()));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return Ok((tools, times));
    }
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&stdout).map_err(|e| format!("bad JSON: {e}"))?;
    for r in rows {
        let sid = match r.get("session_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if !session_ids.is_empty() && !session_ids.contains(&sid) {
            continue;
        }
        if let Some(t) = r.get("time_created").and_then(|v| v.as_i64()) {
            times.entry(sid.clone()).or_default().push(t);
        }
        let data_str = match r.get("data").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };
        let d: serde_json::Value = match serde_json::from_str(data_str) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let is_assistant = d.get("type").and_then(|v| v.as_str()) == Some("assistant")
            || d.get("content").is_some();
        if is_assistant {
            if let Some(contents) = d.get("content").and_then(|v| v.as_array()) {
                for c in contents {
                    if c.get("type").and_then(|v| v.as_str()) == Some("tool") {
                        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let inp = c
                            .get("state")
                            .and_then(|s| s.get("input"))
                            .or_else(|| c.get("input"))
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}));
                        tools.entry(sid.clone()).or_default().push((name.to_string(), inp));
                    }
                }
            }
        }
    }
    Ok((tools, times))
}

pub fn git_outcome(repo: &Path, task: &str) -> Vec<String> {
    let branch = format!("quaqua/{}", task);
    let r = Command::new("git")
        .args([
            "-C",
            &repo.to_string_lossy(),
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{}", branch),
        ])
        .output();
    let out = match r {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let tip = stdout.trim();
    let tip = if tip.len() >= 7 { &tip[..7] } else { tip };
    let merged = Command::new("git")
        .args([
            "-C",
            &repo.to_string_lossy(),
            "merge-base",
            "--is-ancestor",
            &format!("refs/heads/{}", branch),
            "refs/heads/main",
        ])
        .status();
    match merged {
        Ok(s) if s.success() => vec![format!("merged main {}", tip)],
        _ => vec![format!("branch {}", tip)],
    }
}

pub fn runs_outcome(runs_dir: Option<&Path>, task: &str) -> (Vec<String>, Option<String>, u64) {
    let mut tokens = Vec::new();
    let runs_dir = match runs_dir {
        Some(d) if d.is_dir() => d,
        _ => return (tokens, None, 0),
    };

    if runs_dir.join(format!("{}.md", task)).is_file() {
        tokens.push("report".to_string());
    }

    let mut respawned = None;
    for cand in &[
        format!("{}2", task),
        format!("{}3", task),
        format!("{}4", task),
    ] {
        if runs_dir.join(format!("{}.md", cand)).is_file() {
            respawned = Some(cand.clone());
            break;
        }
    }

    let mut rounds = 0;
    let mut slugs = vec![task.to_string()];
    if let Some((_, rest)) = task.split_once('-') {
        slugs.push(rest.to_string());
    }

    if let Ok(entries) = std::fs::read_dir(runs_dir) {
        for entry in entries.flatten() {
            let fn_str = entry.file_name().to_string_lossy().to_string();
            if let Some(stem) = fn_str.strip_suffix(".md") {
                for slug in &slugs {
                    let needle = format!("-{}", slug);
                    if let Some(pos) = stem.find(&needle) {
                        if pos > 0 {
                            let after = &stem[pos + needle.len()..];
                            if let Some(num_str) = after.strip_prefix('r') {
                                let r = if num_str.is_empty() {
                                    1
                                } else {
                                    num_str.parse::<u64>().unwrap_or_default()
                                };
                                if r > 0 {
                                    rounds = rounds.max(r);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    (tokens, respawned, rounds)
}

// ---------------------------------------------------------------- report builder

pub fn build_report(args: &UsageArgs) -> UsageReport {
    let anchor_ms = args.until.unwrap_or_else(now_ms);
    let anchor_vn = anchor_ms + VN_OFFSET_MS;
    let anchor_days = anchor_vn.div_euclid(86_400_000);
    let cutoff_days = anchor_days - (args.days as i64 - 1);
    let cutoff_vn = cutoff_days * 86_400_000;
    let cutoff_ms = cutoff_vn - VN_OFFSET_MS;

    let offline = args.fixture_dir.is_some();
    let mut local_ver = args.local_version.clone();
    let mut notes = Vec::new();
    let mut warn = false;
    let mut logs_error: Option<String> = None;
    let mut logs_partial: Option<String> = None;
    let mut meter_error: Option<String> = None;

    let (raw_rows, meter_raw, db_path, repo) = if let Some(fix_dir) = &args.fixture_dir {
        let rows = match fixture_logs(fix_dir) {
            Ok(r) => r,
            Err(e) => {
                logs_error = Some(e.reason);
                Vec::new()
            }
        };
        let m_raw = match fixture_meter(fix_dir) {
            Ok(m) => Some(m),
            Err(e) => {
                meter_error = Some(e.reason);
                None
            }
        };
        let db = args
            .db
            .clone()
            .unwrap_or_else(|| fix_dir.join("opencode.db"));
        let rep = args.repo.clone().unwrap_or_else(|| fix_dir.clone());
        (rows, m_raw, db, rep)
    } else {
        let token = match get_token() {
            Ok(t) => t,
            Err(e) => {
                logs_error = Some(e.clone());
                meter_error = Some(e);
                String::new()
            }
        };
        if local_ver.is_none() {
            local_ver = local_version();
        }
        let ua = match &local_ver {
            Some(v) => format!("opencode/{}", v),
            None => "opencode".to_string(),
        };
        let rows = if !token.is_empty() {
            match fetch_request_logs(&token, &ua, cutoff_ms, anchor_ms) {
                Ok((r, part)) => {
                    logs_partial = part;
                    r
                }
                Err(e) => {
                    logs_error = Some(e.reason);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let m_raw = if !token.is_empty() {
            match fetch_meter(&token, &ua) {
                Ok(m) => Some(m),
                Err(e) => {
                    meter_error = Some(e.reason);
                    None
                }
            }
        } else {
            None
        };
        let db = args
            .db
            .clone()
            .unwrap_or_else(|| expand_path(DB_DEFAULT));
        let rep = args
            .repo
            .clone()
            .unwrap_or_else(|| expand_path(REPO_DEFAULT));
        (rows, m_raw, db, rep)
    };

    let rows: Vec<serde_json::Value> = raw_rows
        .into_iter()
        .filter(|r| {
            let start = r.get("startedAt").and_then(|v| v.as_i64()).unwrap_or(0);
            cutoff_ms <= start && start <= anchor_ms
        })
        .collect();

    let runs_dir = args.runs_dir.clone().unwrap_or_else(|| repo.join("runs"));

    let mut sessions = BTreeMap::new();
    let db_readable = if db_path.exists() {
        match read_sessions(&db_path) {
            Ok(s) => {
                sessions = s;
                true
            }
            Err(e) => {
                notes.push(format!("local db unreadable: {}", e));
                false
            }
        }
    } else {
        notes.push(format!("local db not found at {}", db_path.display()));
        false
    };

    let mut task_order = Vec::new();
    let mut tasks: BTreeMap<String, InternalTaskEntry> = BTreeMap::new();
    let mut day_order = Vec::new();
    let mut day_stats: BTreeMap<String, DayStat> = BTreeMap::new();
    let mut machine_stats: BTreeMap<String, MachineStat> = BTreeMap::new();

    for r in &rows {
        let sid = r
            .get("sessionID")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let sess = sid.as_ref().and_then(|s| sessions.get(s));
        let task_name = sess.and_then(|s| task_from_directory(s.directory.as_deref()));
        let key = task_name.unwrap_or_else(|| {
            format!(
                "(unknown {})",
                sid.as_ref()
                    .map(|s| if s.len() >= 12 { &s[..12] } else { s })
                    .unwrap_or("no-session")
            )
        });

        if !tasks.contains_key(&key) {
            task_order.push(key.clone());
        }
        let e = tasks.entry(key.clone()).or_insert_with(|| InternalTaskEntry {
            task: key.clone(),
            spend_src: "requests".to_string(),
            sessions: std::collections::BTreeSet::new(),
            requests: 0,
            cost: 0.0,
            tokens: Tokens::default(),
            first: None,
            last: None,
            outcome: Vec::new(),
            idle_min: 0,
        });
        if let Some(s) = &sid {
            e.sessions.insert(s.clone());
        }
        e.requests += 1;
        let c = r.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
        e.cost += c;
        e.tokens.add(&Tokens {
            input: r.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
            output: r.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
            reasoning: r.get("reasoningTokens").and_then(|v| v.as_u64()).unwrap_or(0),
            cache_read: r.get("cacheReadTokens").and_then(|v| v.as_u64()).unwrap_or(0),
        });
        let start = r.get("startedAt").and_then(|v| v.as_i64()).unwrap_or(0);
        let end = r
            .get("lastTokenAt")
            .and_then(|v| v.as_i64())
            .or_else(|| r.get("finishedAt").and_then(|v| v.as_i64()))
            .unwrap_or(start);
        e.first = Some(e.first.map_or(start, |f| f.min(start)));
        e.last = Some(e.last.map_or(end, |l| l.max(end)));

        let dk = day_key(start);
        if !day_stats.contains_key(&dk) {
            day_order.push(dk.clone());
        }
        let d = day_stats.entry(dk.clone()).or_insert_with(|| DayStat {
            date: dk,
            requests: 0,
            cost: 0.0,
            tokens: Tokens::default(),
            first: None,
            last: None,
        });
        d.requests += 1;
        d.cost += c;
        d.tokens.add(&Tokens {
            input: r.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
            output: r.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
            reasoning: r.get("reasoningTokens").and_then(|v| v.as_u64()).unwrap_or(0),
            cache_read: r.get("cacheReadTokens").and_then(|v| v.as_u64()).unwrap_or(0),
        });
        d.first = Some(d.first.map_or(start, |f| f.min(start)));
        d.last = Some(d.last.map_or(end, |l| l.max(end)));

        let ua = r
            .get("userAgent")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let m = machine_stats
            .entry(ua.clone())
            .or_insert_with(|| MachineStat {
                ua,
                version: None,
                requests: 0,
                cost: 0.0,
                local: false,
            });
        m.requests += 1;
        m.cost += c;
    }

    let mut waste = Waste::default();
    let sessions_used: std::collections::HashSet<String> = tasks
        .values()
        .flat_map(|t| t.sessions.iter().cloned())
        .collect();

    let (tools, msg_times) = if db_readable && !sessions_used.is_empty() {
        read_messages(&db_path, &sessions_used).unwrap_or_default()
    } else {
        (BTreeMap::new(), BTreeMap::new())
    };

    for key in &task_order {
        if let Some(e) = tasks.get_mut(key) {
            let mut toks = Vec::new();
            if !e.sessions.is_empty() {
                let idles: Vec<Option<String>> = e
                    .sessions
                    .iter()
                    .filter_map(|s| sessions.get(s).and_then(|info| info.idle_outcome.clone()))
                    .map(Some)
                    .collect();
                for bad in idles.iter().flatten() {
                    if bad == "failed" || bad == "interrupted" {
                        toks.push(format!("killed({})", bad));
                        waste.killed.push(WasteKilled {
                            task: key.clone(),
                            idle_outcome: bad.clone(),
                        });
                        break;
                    }
                }
            }
            if repo.join(".git").is_dir() {
                toks.extend(git_outcome(&repo, key));
            }
            let (r_toks, respawned, rounds) = runs_outcome(Some(&runs_dir), key);
            toks.extend(r_toks);
            if let Some(resp) = respawned {
                waste.respawn.push(WasteRespawn {
                    task: key.clone(),
                    replaced_by: resp,
                });
            }
            if rounds > 0 {
                waste.review_rounds.push(WasteReview {
                    task: key.clone(),
                    rounds,
                });
            }
            e.outcome = if toks.is_empty() {
                vec!["?".to_string()]
            } else {
                toks
            };

            let mut gaps = Vec::new();
            for sid in &e.sessions {
                if let Some(ts_list) = msg_times.get(sid) {
                    let mut sorted_ts = ts_list.clone();
                    sorted_ts.sort_unstable();
                    for w in sorted_ts.windows(2) {
                        if w[1] - w[0] > 30 * 60 * 1000 {
                            gaps.push(w[1] - w[0]);
                        }
                    }
                }
            }
            if !gaps.is_empty() {
                let idle_min = (gaps.iter().sum::<i64>() / 60000) as u64;
                waste.idle_gt_30m.push(WasteIdle {
                    task: key.clone(),
                    idle_min,
                });
                e.idle_min = idle_min;
            }
        }
    }
    for e in tasks.values_mut() {
        let cwd = e
            .sessions
            .iter()
            .find_map(|sid| sessions.get(sid).and_then(|info| info.directory.as_deref()))
            .unwrap_or("");
        let sid = if e.sessions.len() == 1 {
            e.sessions.iter().next().map(|s| s.as_str()).unwrap_or("")
        } else {
            ""
        };
        let is_in_db = e.sessions.iter().any(|s| sessions.contains_key(s));
        let harness_hint = if is_in_db { "opencode" } else { "" };
        let spend = crate::worker::prompt::cost::read_worker_spend_with(
            &e.task,
            sid,
            cwd,
            harness_hint,
            None,
            Some(&db_path),
            Some(&runs_dir),
            false,
        );
        if let Some(usd) = spend.usd {
            e.cost = usd;
            e.spend_src = "session".to_string();
        }
        if let Some(toks) = spend.tokens {
            e.tokens = Tokens {
                input: toks.in_tokens,
                output: toks.out_tokens,
                reasoning: toks.reasoning,
                cache_read: toks.cache_read,
            };
        }
    }

    let mut sess_cost: BTreeMap<String, f64> = BTreeMap::new();
    for t in tasks.values() {
        let share = if !t.sessions.is_empty() {
            t.cost / t.sessions.len() as f64
        } else {
            0.0
        };
        for sid in &t.sessions {
            *sess_cost.entry(sid.clone()).or_insert(0.0) += share;
        }
    }

    let mut split_calls: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut split_cost: BTreeMap<&'static str, f64> = BTreeMap::new();
    for &cat in &["product", "process", "other"] {
        split_calls.insert(cat, 0);
        split_cost.insert(cat, 0.0);
    }

    for (sid, calls) in &tools {
        if calls.is_empty() {
            continue;
        }
        let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
        for (n, inp) in calls {
            let cls = classify_tool(n, inp);
            *counts.entry(cls).or_insert(0) += 1;
        }
        let total: u64 = counts.values().sum();
        let total = total.max(1);
        let s_cost = sess_cost.get(sid).copied().unwrap_or(0.0);
        for (cls, n) in counts {
            *split_calls.entry(cls).or_default() += n;
            *split_cost.entry(cls).or_default() += s_cost * (n as f64) / (total as f64);
        }
    }
    for sid in &sessions_used {
        if !tools.contains_key(sid) || tools[sid].is_empty() {
            let s_cost = sess_cost.get(sid).copied().unwrap_or(0.0);
            *split_cost.entry("other").or_default() += s_cost;
        }
    }

    let split_total_calls: u64 = split_calls.values().sum::<u64>().max(1);
    let split_total_cost: f64 = {
        let s = split_cost.values().sum::<f64>();
        if s == 0.0 {
            1.0
        } else {
            s
        }
    };

    let mut split: BTreeMap<String, SplitCategory> = BTreeMap::new();
    for &cat in &["product", "process", "other"] {
        let calls = split_calls[cat];
        let cost = split_cost[cat];
        split.insert(
            cat.to_string(),
            SplitCategory {
                calls_pct: 100.0 * (calls as f64) / (split_total_calls as f64),
                cost_pct: 100.0 * cost / split_total_cost,
                calls,
                cost: round4(cost),
            },
        );
    }

    let mut meter = BTreeMap::new();
    if let Some(m_obj) = meter_raw.as_ref().and_then(|v| v.get("usage")).and_then(|v| v.as_object()) {
        for (win, w) in m_obj {
            let resets = w.get("resetsAt").and_then(|v| v.as_str()).map(|s| s.to_string());
            let mut start_ms: Option<i64> = None;
            if let Some(r_str) = &resets {
                if let Some(rdt_ms) = parse_iso_utc(r_str) {
                    let s_ms = if win == "rolling" {
                        rdt_ms - 5 * 3600 * 1000
                    } else if win == "weekly" {
                        rdt_ms - 7 * 86_400 * 1000
                    } else if win == "monthly" {
                        let days = rdt_ms.div_euclid(86_400_000);
                        let rem = rdt_ms.rem_euclid(86_400_000);
                        let (y, m, d) = civil_from_days(days);
                        let (prev_y, prev_m) = if m == 1 {
                            (y - 1, 12)
                        } else {
                            (y, m - 1)
                        };
                        let prev_days = days_from_civil(prev_y, prev_m, d);
                        prev_days * 86_400_000 + rem
                    } else {
                        rdt_ms
                    };
                    start_ms = Some(s_ms);
                }
            }
            let percent = w.get("percent").and_then(|v| v.as_i64());
            let status = w.get("status").and_then(|v| v.as_str()).map(|s| s.to_string());
            let mut entry = MeterWindow {
                percent,
                status,
                resets_at: resets,
                window_start: None,
                log_cost: None,
                range_complete: None,
            };
            if let Some(s_ms) = start_ms {
                entry.window_start = Some(format_iso_vn(s_ms));
                let lo = s_ms.max(cutoff_ms);
                let log_cost: f64 = rows
                    .iter()
                    .filter(|r| {
                        let st = r.get("startedAt").and_then(|v| v.as_i64()).unwrap_or(0);
                        lo <= st && st <= anchor_ms
                    })
                    .map(|r| r.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0))
                    .sum();
                entry.log_cost = Some(round4(log_cost));
                entry.range_complete = Some(s_ms >= cutoff_ms);
            }
            meter.insert(win.clone(), entry);
        }
    }

    let mut machines = Vec::new();
    let mut sorted_machines: Vec<MachineStat> = machine_stats.into_values().collect();
    sorted_machines.sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal));
    for mut m in sorted_machines {
        let ver = extract_version(&m.ua);
        let is_local = local_ver.is_some() && ver.as_deref() == local_ver.as_deref();
        if !is_local && local_ver.is_some() {
            warn = true;
        }
        if ver.is_none() && local_ver.is_some() {
            warn = true;
        }
        m.version = ver;
        m.local = is_local;
        m.cost = round4(m.cost);
        machines.push(m);
    }

    let total_cost: f64 = tasks.values().map(|t| t.cost).sum();
    let total_req: u64 = tasks.values().map(|t| t.requests).sum();
    let mut total_tokens = Tokens::default();
    for t in tasks.values() {
        total_tokens.add(&t.tokens);
    }

    if let Some(err) = &logs_error {
        notes.push(format!(
            "request-logs UNAVAILABLE: {} — report has no request data",
            err
        ));
    } else if let Some(part) = &logs_partial {
        notes.push(format!("request-logs partial: {}", part));
    }
    if let Some(err) = &meter_error {
        notes.push(format!("meter unavailable: {}", err));
    }

    let mut final_tasks: Vec<TaskStat> = tasks
        .into_values()
        .map(|t| TaskStat {
            task: t.task,
            spend_src: t.spend_src,
            sessions: t.sessions.into_iter().collect(),
            requests: t.requests,
            cost: round4(t.cost),
            tokens: t.tokens,
            wall_min: if let Some(first) = t.first {
                ((t.last.unwrap_or(first) - first) / 60000) as u64
            } else {
                0
            },
            idle_min: t.idle_min,
            outcome: t.outcome.join("; "),
        })
        .collect();
    final_tasks.sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal));

    let days_table: Vec<DayStat> = day_order
        .into_iter()
        .filter_map(|dk| {
            day_stats.remove(&dk).map(|mut d| {
                d.cost = round4(d.cost);
                d
            })
        })
        .collect();

    let exit_code = if logs_error.is_some() { 2 } else { 0 };

    UsageReport {
        generated: format_iso_vn(now_ms()),
        offline,
        days: args.days,
        until: format_iso_vn(anchor_ms),
        cutoff: format_iso_vn(cutoff_ms),
        local_version: local_ver,
        totals: Totals {
            requests: total_req,
            cost: round4(total_cost),
            tokens: total_tokens,
        },
        warn_other_machine: warn,
        machines,
        days_table,
        tasks: final_tasks,
        split,
        waste,
        meter,
        meter_unavailable: meter_error,
        request_logs_error: logs_error,
        request_logs_partial: logs_partial,
        notes,
        exit_code,
    }
}

struct InternalTaskEntry {
    task: String,
    spend_src: String,
    sessions: std::collections::BTreeSet<String>,
    requests: u64,
    cost: f64,
    tokens: Tokens,
    first: Option<i64>,
    last: Option<i64>,
    outcome: Vec<String>,
    idle_min: u64,
}

// ---------------------------------------------------------------- text render

pub fn render_text(rep: &UsageReport) -> Result<String, String> {
    let mut lines = Vec::new();
    let cutoff_short = if rep.cutoff.len() >= 10 {
        &rep.cutoff[..10]
    } else {
        &rep.cutoff
    };
    let until_short = if rep.until.len() >= 10 {
        &rep.until[..10]
    } else {
        &rep.until
    };
    let gen_short = if rep.generated.len() >= 16 {
        &rep.generated[..16]
    } else {
        &rep.generated
    };

    lines.push(format!(
        "Usage audit — {} → {} ({} VN days) · generated {}",
        cutoff_short, until_short, rep.days, gen_short
    ));
    if rep.offline {
        lines.push("(offline fixture mode)".to_string());
    }
    let t = &rep.totals;
    lines.push(format!(
        "Local opencode: {} · {} requests · ${:.4} (task-session $) · in {} / out {} / rsn {} / cacheRead {}",
        rep.local_version.as_deref().unwrap_or("?"),
        t.requests,
        t.cost,
        kfmt(t.tokens.input),
        kfmt(t.tokens.output),
        kfmt(t.tokens.reasoning),
        kfmt(t.tokens.cache_read)
    ));

    if let Some(err) = &rep.request_logs_error {
        lines.push(format!(
            "[UNAVAILABLE] request-logs: {} — no request data in this report",
            err
        ));
    } else if let Some(part) = &rep.request_logs_partial {
        lines.push(format!("[PARTIAL] request-logs: {}", part));
    }
    if let Some(err) = &rep.meter_unavailable {
        lines.push(format!("[UNAVAILABLE] meter: {}", err));
    }
    if rep.warn_other_machine {
        lines.push(format!(
            "[WARN] requests seen with a UA version different from local {} — possibly another machine:",
            rep.local_version.as_deref().unwrap_or("?")
        ));
        for m in &rep.machines {
            if !m.local {
                lines.push(format!("       {}: {} req · ${:.4}", m.ua, m.requests, m.cost));
            }
        }
    }

    lines.push(String::new());
    lines.push("== Per day ==".to_string());
    lines.push(format!(
        "{:<6} {:>6} {:>9} {:>8} {:>8} {:>8} {:>10} {:>7}",
        "day", "req", "$", "in", "out", "rsn", "cacheRead", "wall"
    ));
    for d in &rep.days_table {
        let wall = if let Some(f) = d.first {
            (d.last.unwrap_or(f) - f) / 60000
        } else {
            0
        };
        lines.push(format!(
            "{:<6} {:>6} {:>9.4} {:>8} {:>8} {:>8} {:>10} {:>6}m",
            d.date,
            d.requests,
            d.cost,
            kfmt(d.tokens.input),
            kfmt(d.tokens.output),
            kfmt(d.tokens.reasoning),
            kfmt(d.tokens.cache_read),
            wall
        ));
    }

    lines.push(String::new());
    lines.push("== Per task (task cost = whole session, matching watchdog; day rows = window) ==".to_string());
    lines.push(format!(
        "{:<26} {:<8} {:>5} {:>9} {:>7} {:>7} {:>6} {:>9} {:>6} {:<34} {}",
        "task", "src", "req", "$", "in", "out", "rsn", "cacheRead", "wall", "outcome", "flags"
    ));
    for task in &rep.tasks {
        let mut flags = Vec::new();
        for w in &rep.waste.respawn {
            if w.task == task.task {
                flags.push(format!("respawn→{}", w.replaced_by));
            }
        }
        for w in &rep.waste.killed {
            if w.task == task.task {
                flags.push("killed".to_string());
            }
        }
        for w in &rep.waste.idle_gt_30m {
            if w.task == task.task {
                flags.push(format!("idle {}m", w.idle_min));
            }
        }
        for w in &rep.waste.review_rounds {
            if w.task == task.task {
                flags.push(format!("review r{}", w.rounds));
            }
        }
        let t_display = if task.task.len() > 26 {
            &task.task[..26]
        } else {
            &task.task
        };
        let out_display = if task.outcome.len() > 34 {
            &task.outcome[..34]
        } else {
            &task.outcome
        };
        lines.push(format!(
            "{:<26} {:<8} {:>5} {:>9.4} {:>7} {:>7} {:>6} {:>9} {:>5}m {:<34} {}",
            t_display,
            task.spend_src,
            task.requests,
            task.cost,
            kfmt(task.tokens.input),
            kfmt(task.tokens.output),
            kfmt(task.tokens.reasoning),
            kfmt(task.tokens.cache_read),
            task.wall_min,
            out_display,
            flags.join(",")
        ));
    }

    lines.push(String::new());
    lines.push("== Product vs process (tool calls; $ weighted per session) ==".to_string());
    for &cls in &["product", "process", "other"] {
        if let Some(s) = rep.split.get(cls) {
            lines.push(format!(
                "{:<8} {:>5.0}% calls · {:>5.0}% $  ({} calls · ${:.4})",
                cls, s.calls_pct, s.cost_pct, s.calls, s.cost
            ));
        }
    }

    lines.push(String::new());
    lines.push("== Waste flags ==".to_string());
    for w in &rep.waste.respawn {
        lines.push(format!("respawn     {} → {}", w.task, w.replaced_by));
    }
    for w in &rep.waste.killed {
        lines.push(format!(
            "killed      {} (session idle outcome: {})",
            w.task, w.idle_outcome
        ));
    }
    for w in &rep.waste.idle_gt_30m {
        lines.push(format!("idle>30m    {} ({}m)", w.task, w.idle_min));
    }
    for w in &rep.waste.review_rounds {
        lines.push(format!("review      {} ({} round(s))", w.task, w.rounds));
    }
    if rep.waste.is_empty() {
        lines.push("none".to_string());
    }

    lines.push(String::new());
    lines.push("== Meter (Go, /inference/go/v1/usage) ==".to_string());
    if let Some(err) = &rep.meter_unavailable {
        lines.push(format!("unavailable: {}", err));
    }
    for &win in &["rolling", "weekly", "monthly"] {
        if let Some(m) = rep.meter.get(win) {
            let mut extra = String::new();
            if let Some(lc) = m.log_cost {
                let complete_str = if m.range_complete.unwrap_or(false) {
                    ""
                } else {
                    " (range incomplete)"
                };
                extra = format!(" · log $ in window: ${:.4}{}", lc, complete_str);
            }
            let pct_str = m.percent.map_or("?".to_string(), |p| p.to_string());
            let resets_str = m.resets_at.as_deref().unwrap_or("?");
            let resets_short = if resets_str.len() >= 16 {
                &resets_str[..16]
            } else {
                resets_str
            };
            lines.push(format!(
                "{:<8} {}% · resets {}{}",
                win, pct_str, resets_short, extra
            ));
        }
    }

    lines.push(String::new());
    lines.push("== Machines (UA) ==".to_string());
    for m in &rep.machines {
        let suffix = if m.local {
            "  (local)".to_string()
        } else {
            format!(
                "  [≠ local {}]",
                rep.local_version.as_deref().unwrap_or("?")
            )
        };
        lines.push(format!(
            "{:<34} {:>5} req · ${:.4}{}",
            m.ua, m.requests, m.cost, suffix
        ));
    }

    if !rep.notes.is_empty() {
        lines.push(String::new());
        lines.push("== Notes ==".to_string());
        for n in &rep.notes {
            lines.push(format!("- {}", n));
        }
    }

    let out = lines.join("\n");
    if contains_token(&out) {
        return Err("refusing to print output that looks like a token".to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------- runner

pub fn handle_usage(raw_args: &[String]) -> Result<i32, String> {
    let mut clap_args = vec!["usage".to_string()];
    clap_args.extend_from_slice(raw_args);

    let args = UsageArgs::try_parse_from(clap_args).map_err(|e| e.to_string())?;
    let rep = build_report(&args);

    if args.json {
        let json_str = serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?;
        if contains_token(&json_str) {
            return Err("refusing to print output that looks like a token".to_string());
        }
        println!("{}", json_str);
    } else {
        let text = render_text(&rep)?;
        println!("{}", text);
    }

    Ok(rep.exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_sessions_rusqlite() {
        let tmp = std::env::temp_dir().join(format!("read-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let db_path = tmp.join("test.db");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_v2 (
                id TEXT PRIMARY KEY,
                directory TEXT,
                idle_outcome TEXT
            );
            INSERT INTO session_v2 VALUES ('s1', '/path/to/.wt/worker-a', 'succeeded');
            INSERT INTO session_v2 VALUES ('s2', '/path/to/.wt/worker-b', 'failed');",
        )
        .unwrap();
        drop(conn);

        let sessions = read_sessions(&db_path).expect("read sessions");
        assert_eq!(sessions.len(), 2);
        assert_eq!(
            sessions["s1"].directory.as_deref(),
            Some("/path/to/.wt/worker-a")
        );
        assert_eq!(sessions["s1"].idle_outcome.as_deref(), Some("succeeded"));
        assert_eq!(
            sessions["s2"].directory.as_deref(),
            Some("/path/to/.wt/worker-b")
        );
        assert_eq!(sessions["s2"].idle_outcome.as_deref(), Some("failed"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
