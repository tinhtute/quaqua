//! Agent liveness with "absence ≠ broken" (issue #69).
//!
//! One failed `herdr agent get` proves nothing: a timeout, a socket error or
//! an empty `agent list` is `Unknown`, never `Down`. `Down` needs two
//! independent checks that agree: `agent get` says `agent_not_found` and
//! `agent list` parses, is non-empty, and does not name the agent.

use super::HerdrClient;
use serde_json::Value;
use std::path::Path;

/// Default staleness threshold for hook-reported state (10 minutes, matching PROMPT_WORKER_STALL_SEC).
pub const DEFAULT_STATE_STALENESS_SEC: u64 = 600;

/// Normalized agent/worker state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerState {
    Idle,
    Blocked,
    Working,
    Done,
    Dead,
    Unknown(String),
}

impl WorkerState {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "idle" => WorkerState::Idle,
            "blocked" => WorkerState::Blocked,
            "working" => WorkerState::Working,
            "done" => WorkerState::Done,
            "dead" | "gone" => WorkerState::Dead,
            other => WorkerState::Unknown(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            WorkerState::Idle => "idle",
            WorkerState::Blocked => "blocked",
            WorkerState::Working => "working",
            WorkerState::Done => "done",
            WorkerState::Dead => "dead",
            WorkerState::Unknown(s) => {
                if s.is_empty() {
                    "unknown"
                } else {
                    s.as_str()
                }
            }
        }
    }
}

impl std::fmt::Display for WorkerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A hook-reported worker state token + timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookState {
    pub state: WorkerState,
    pub timestamp_sec: u64,
}

impl HookState {
    pub fn new(state: WorkerState, timestamp_sec: u64) -> Self {
        Self {
            state,
            timestamp_sec,
        }
    }

    /// Check if this hook state has expired based on current timestamp and staleness bound.
    /// If `staleness_bound_sec == 0`, staleness is disabled (always fresh).
    pub fn is_stale(&self, now_sec: u64, staleness_bound_sec: u64) -> bool {
        if staleness_bound_sec == 0 {
            return false;
        }
        if self.timestamp_sec == 0 {
            return false;
        }
        if now_sec > self.timestamp_sec {
            now_sec - self.timestamp_sec > staleness_bound_sec
        } else {
            false
        }
    }

    /// Parse a hook state from content (supports "<state> <ts>", "state=<st>\nts=<ts>", or JSON).
    pub fn parse(content: &str) -> Option<Self> {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                let state_str = v.get("state").and_then(|s| s.as_str())?;
                let ts = v
                    .get("timestamp")
                    .or_else(|| v.get("ts"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0);
                return Some(Self {
                    state: WorkerState::parse(state_str),
                    timestamp_sec: ts,
                });
            }
        }
        if trimmed.contains("state=") {
            let mut state = None;
            let mut ts = 0;
            for line in trimmed.lines() {
                let l = line.trim();
                if let Some(s) = l.strip_prefix("state=") {
                    state = Some(WorkerState::parse(s));
                } else if let Some(t) = l
                    .strip_prefix("ts=")
                    .or_else(|| l.strip_prefix("timestamp="))
                {
                    ts = t.parse::<u64>().unwrap_or(0);
                }
            }
            if let Some(st) = state {
                return Some(Self {
                    state: st,
                    timestamp_sec: ts,
                });
            }
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }
        let state = WorkerState::parse(parts[0]);
        let ts = if parts.len() > 1 {
            parts[1].parse::<u64>().unwrap_or(0)
        } else {
            0
        };
        Some(Self {
            state,
            timestamp_sec: ts,
        })
    }
}

/// The core decision function: resolves an agent's status.
///
/// What the agent reports of itself via hooks wins when fresh (`working` or `blocked`).
/// Screen/pane inference counts when there is no hook state (non-Claude harnesses,
/// state file missing or stale), or for terminal/menu conditions (`done` or `blocked` on screen).
pub fn status_from(
    hook_state: Option<&HookState>,
    screen_status: &str,
    now_sec: u64,
    staleness_bound_sec: u64,
) -> WorkerState {
    let screen = WorkerState::parse(screen_status);
    if screen == WorkerState::Dead {
        return WorkerState::Dead;
    }
    let Some(said) = hook_state else {
        return screen;
    };
    if said.is_stale(now_sec, staleness_bound_sec) {
        return screen;
    }
    match said.state {
        WorkerState::Working => WorkerState::Working,
        WorkerState::Blocked => WorkerState::Blocked,
        WorkerState::Idle => {
            if screen == WorkerState::Done || screen == WorkerState::Blocked {
                screen
            } else {
                WorkerState::Idle
            }
        }
        ref other => other.clone(),
    }
}

/// Read the hook state from `runs_dir/.<name>.state` if present.
pub fn read_hook_state(runs_dir: &Path, name: &str) -> Option<HookState> {
    let state_file = runs_dir.join(format!(".{name}.state"));
    let content = std::fs::read_to_string(state_file).ok()?;
    HookState::parse(&content)
}

/// Resolve worker state by inspecting `runs_dir/.<name>.state` first,
/// falling back to `screen_status`.
pub fn resolve_worker_state(
    runs_dir: &Path,
    name: &str,
    screen_status: &str,
    now_sec: u64,
    staleness_bound_sec: u64,
) -> WorkerState {
    let hook = read_hook_state(runs_dir, name);
    status_from(hook.as_ref(), screen_status, now_sec, staleness_bound_sec)
}

/// Determine a worker's state by combining herdr liveness / agent_get with hook state.
pub fn worker_liveness_state<H: HerdrClient>(
    herdr: &H,
    runs_dir: &Path,
    name: &str,
    now_sec: u64,
    staleness_bound_sec: u64,
) -> WorkerState {
    let live = liveness(herdr, name);
    match live {
        Liveness::Down => WorkerState::Dead,
        Liveness::Alive(body) => {
            let screen_status = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.pointer("/result/agent/agent_status")
                        .or_else(|| v.pointer("/result/agent/status"))
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .unwrap_or_else(|| "unknown".to_string());
            resolve_worker_state(runs_dir, name, &screen_status, now_sec, staleness_bound_sec)
        }
        Liveness::Unknown(why) => {
            if let Some(hook) = read_hook_state(runs_dir, name) {
                if !hook.is_stale(now_sec, staleness_bound_sec) {
                    return hook.state;
                }
            }
            WorkerState::Unknown(why)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// `agent get` returned `result.agent`; carries the raw body.
    Alive(String),
    /// Confirmed gone by `agent get` and `agent list`.
    Down,
    /// Could not prove either; carries the reason.
    Unknown(String),
}

pub fn liveness<H: HerdrClient>(herdr: &H, name: &str) -> Liveness {
    let (ok, body) = match herdr.agent_get(name) {
        Ok(r) => (r.ok, r.body),
        Err(e) => return Liveness::Unknown(format!("herdr agent get failed: {e}")),
    };
    let json = serde_json::from_str::<Value>(&body).ok();
    if ok
        && json
            .as_ref()
            .and_then(|j| j.pointer("/result/agent"))
            .is_some()
    {
        return Liveness::Alive(body);
    }
    let not_found = json
        .as_ref()
        .and_then(|j| j.pointer("/error/code"))
        .and_then(Value::as_str)
        == Some("agent_not_found")
        || body.contains("agent_not_found");
    if !not_found {
        return Liveness::Unknown(format!("herdr agent get failed: {}", body.trim()));
    }
    // Second, independent check before calling it gone.
    let list = match herdr.agent_list() {
        Ok(r) => r.body,
        Err(e) => {
            return Liveness::Unknown(format!("agent_not_found, but herdr agent list failed: {e}"))
        }
    };
    let names: Option<Vec<String>> = serde_json::from_str::<Value>(&list)
        .ok()
        .and_then(|v| {
            v.pointer("/result/agents")
                .and_then(Value::as_array)
                .cloned()
        })
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        });
    match names {
        None => {
            Liveness::Unknown("agent_not_found, but herdr agent list is unreadable".to_string())
        }
        Some(n) if n.is_empty() => {
            Liveness::Unknown("agent_not_found, but herdr agent list is empty".to_string())
        }
        Some(n) if n.iter().any(|x| x == name) => {
            Liveness::Unknown("agent_not_found, but herdr agent list still names it".to_string())
        }
        Some(_) => Liveness::Down,
    }
}
