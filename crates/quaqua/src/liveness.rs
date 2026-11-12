//! Agent liveness with "absence ≠ broken" (issue #69).
//!
//! One failed `herdr agent get` proves nothing: a timeout, a socket error or
//! an empty `agent list` is `Unknown`, never `Down`. `Down` needs two
//! independent checks that agree: `agent get` says `agent_not_found` and
//! `agent list` parses, is non-empty, and does not name the agent.

use crate::herdr::HerdrOps;
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
    let state_file = crate::state::WorkerMetadata::state_path(runs_dir, name);
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
pub fn worker_liveness_state<H: HerdrOps>(
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

pub fn liveness<H: HerdrOps>(herdr: &H, name: &str) -> Liveness {
    let (ok, body) = match herdr.agent_get(name) {
        Ok(r) => r,
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
        Ok(s) => s,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::process::Output;

    struct Fake {
        get: io::Result<(bool, String)>,
        list: io::Result<String>,
    }

    fn clone_res<T: Clone>(r: &io::Result<T>) -> io::Result<T> {
        match r {
            Ok(v) => Ok(v.clone()),
            Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
        }
    }

    impl HerdrOps for Fake {
        fn run(&self, _: &[&str]) -> io::Result<Output> {
            unimplemented!()
        }
        fn agent_list(&self) -> io::Result<String> {
            clone_res(&self.list)
        }
        fn agent_get(&self, _: &str) -> io::Result<(bool, String)> {
            clone_res(&self.get)
        }
        fn agent_read(&self, _: &str, _: &str, _: u32) -> io::Result<String> {
            unimplemented!()
        }
        fn agent_start(&self, _: &str, _: &str, _: &str, _: u32, _: &[&str]) -> io::Result<Output> {
            unimplemented!()
        }
        fn pane_split(&self, _: &str, _: &str, _: &[(&str, &str)]) -> io::Result<Output> {
            unimplemented!()
        }
        fn pane_close(&self, _: &str) -> io::Result<Output> {
            unimplemented!()
        }
        fn pane_report_metadata(&self, _: &str, _: &[&str]) -> io::Result<Output> {
            unimplemented!()
        }
        fn pane_wait_output(&self, _: &str, _: &str, _: u32) -> bool {
            unimplemented!()
        }
        fn pane_read(&self, _: &str, _: &str, _: u32) -> io::Result<String> {
            unimplemented!()
        }
        fn pane_send_keys(&self, _: &str, _: &str) -> io::Result<Output> {
            unimplemented!()
        }
        fn agent_send_keys(&self, _: &str, _: &str) -> io::Result<Output> {
            unimplemented!()
        }
        fn agent_wait_idle(&self, _: &str, _: u32) -> io::Result<Output> {
            unimplemented!()
        }
        fn agent_prompt_nowait(&self, _: &str, _: &str) -> io::Result<Output> {
            unimplemented!()
        }
        fn agent_prompt_wait_background(
            &self,
            _: &str,
            _: &str,
            _: u32,
            _: std::fs::File,
        ) -> io::Result<std::process::Child> {
            unimplemented!()
        }
    }

    const ALIVE: &str = r#"{"result":{"agent":{"name":"w","pane_id":"%1"}}}"#;
    const NOT_FOUND: &str = r#"{"error":{"code":"agent_not_found"}}"#;
    const LIST_OTHER: &str = r#"{"result":{"agents":[{"name":"cos"}]}}"#;
    const LIST_W: &str = r#"{"result":{"agents":[{"name":"w"}]}}"#;
    const LIST_EMPTY: &str = r#"{"result":{"agents":[]}}"#;

    fn fake(get: io::Result<(bool, String)>, list: io::Result<String>) -> Fake {
        Fake { get, list }
    }

    #[test]
    fn alive_when_get_returns_agent() {
        let f = fake(Ok((true, ALIVE.into())), Ok(LIST_W.into()));
        assert_eq!(liveness(&f, "w"), Liveness::Alive(ALIVE.into()));
    }

    #[test]
    fn down_only_when_list_confirms_not_found() {
        let f = fake(Ok((false, NOT_FOUND.into())), Ok(LIST_OTHER.into()));
        assert_eq!(liveness(&f, "w"), Liveness::Down);
    }

    #[test]
    fn not_found_but_listed_is_unknown() {
        let f = fake(Ok((false, NOT_FOUND.into())), Ok(LIST_W.into()));
        assert!(matches!(liveness(&f, "w"), Liveness::Unknown(_)));
    }

    #[test]
    fn get_timeout_is_unknown() {
        let f = fake(
            Err(io::Error::new(io::ErrorKind::TimedOut, "timed out")),
            Ok(LIST_OTHER.into()),
        );
        assert!(matches!(liveness(&f, "w"), Liveness::Unknown(_)));
    }

    #[test]
    fn empty_or_broken_list_is_unknown() {
        for list in [
            Ok(LIST_EMPTY.to_string()),
            Ok("garbage".to_string()),
            Err(io::Error::other("socket")),
        ] {
            let f = fake(Ok((false, NOT_FOUND.into())), list);
            assert!(matches!(liveness(&f, "w"), Liveness::Unknown(_)));
        }
    }

    #[test]
    fn other_get_error_is_unknown() {
        let f = fake(
            Ok((false, r#"{"error":{"code":"socket_error"}}"#.into())),
            Ok(LIST_OTHER.into()),
        );
        assert!(matches!(liveness(&f, "w"), Liveness::Unknown(_)));
    }

    #[test]
    fn hook_state_parsing_formats() {
        // Plain format: "<state> <ts>"
        let h1 = HookState::parse("working 1730000000").expect("parse plain");
        assert_eq!(h1.state, WorkerState::Working);
        assert_eq!(h1.timestamp_sec, 1730000000);

        let h2 = HookState::parse("blocked 1730000010").expect("parse blocked");
        assert_eq!(h2.state, WorkerState::Blocked);
        assert_eq!(h2.timestamp_sec, 1730000010);

        let h3 = HookState::parse("idle 1730000020").expect("parse idle");
        assert_eq!(h3.state, WorkerState::Idle);
        assert_eq!(h3.timestamp_sec, 1730000020);

        // Key-value lines
        let h4 = HookState::parse("state=working\nts=1730000030\n").expect("parse kv");
        assert_eq!(h4.state, WorkerState::Working);
        assert_eq!(h4.timestamp_sec, 1730000030);

        // JSON format
        let h5 = HookState::parse(r#"{"state": "blocked", "timestamp": 1730000040}"#)
            .expect("parse json");
        assert_eq!(h5.state, WorkerState::Blocked);
        assert_eq!(h5.timestamp_sec, 1730000040);

        // State only without timestamp
        let h6 = HookState::parse("working").expect("parse bare");
        assert_eq!(h6.state, WorkerState::Working);
        assert_eq!(h6.timestamp_sec, 0);

        // Empty / whitespace
        assert!(HookState::parse("").is_none());
        assert!(HookState::parse("   \n\t  ").is_none());
    }

    #[test]
    fn hook_state_staleness_expiration() {
        let h = HookState::new(WorkerState::Working, 1000);
        // Bound = 600s
        assert!(!h.is_stale(1000, 600)); // age 0
        assert!(!h.is_stale(1500, 600)); // age 500
        assert!(!h.is_stale(1600, 600)); // age 600 (boundary)
        assert!(h.is_stale(1601, 600)); // age 601 (expired)
        assert!(h.is_stale(2000, 600)); // age 1000 (expired)

        // Bound = 0 disables staleness
        assert!(!h.is_stale(999999, 0));

        // Future timestamp (clock skew)
        assert!(!h.is_stale(900, 600));
    }

    #[test]
    fn status_from_derives_state_from_fresh_hook_events() {
        let now = 1000;
        let bound = 600;

        // 1. Hook says working: overrides screen (even when screen says idle or unknown)
        let working_hook = HookState::new(WorkerState::Working, 950);
        assert_eq!(
            status_from(Some(&working_hook), "idle", now, bound),
            WorkerState::Working
        );
        assert_eq!(
            status_from(Some(&working_hook), "unknown", now, bound),
            WorkerState::Working
        );

        // 2. Hook says blocked: overrides screen (even when screen says working or idle)
        let blocked_hook = HookState::new(WorkerState::Blocked, 950);
        assert_eq!(
            status_from(Some(&blocked_hook), "working", now, bound),
            WorkerState::Blocked
        );
        assert_eq!(
            status_from(Some(&blocked_hook), "idle", now, bound),
            WorkerState::Blocked
        );

        // 3. Hook says idle: overrides screen working false-positive (pane mis-tag)
        let idle_hook = HookState::new(WorkerState::Idle, 950);
        assert_eq!(
            status_from(Some(&idle_hook), "working", now, bound),
            WorkerState::Idle
        );
        assert_eq!(
            status_from(Some(&idle_hook), "unknown", now, bound),
            WorkerState::Idle
        );

        // 4. Hook says idle but screen sees done or blocked (shepherdr terminal/menu condition)
        assert_eq!(
            status_from(Some(&idle_hook), "done", now, bound),
            WorkerState::Done
        );
        assert_eq!(
            status_from(Some(&idle_hook), "blocked", now, bound),
            WorkerState::Blocked
        );

        // 5. Screen confirmed dead/gone dominates
        assert_eq!(
            status_from(Some(&working_hook), "dead", now, bound),
            WorkerState::Dead
        );
        assert_eq!(
            status_from(Some(&working_hook), "gone", now, bound),
            WorkerState::Dead
        );
    }

    #[test]
    fn status_from_falls_back_to_screen_without_state_file() {
        let now = 1000;
        let bound = 600;

        // No hook state: screen status is used directly
        assert_eq!(
            status_from(None, "working", now, bound),
            WorkerState::Working
        );
        assert_eq!(status_from(None, "idle", now, bound), WorkerState::Idle);
        assert_eq!(
            status_from(None, "blocked", now, bound),
            WorkerState::Blocked
        );
        assert_eq!(status_from(None, "done", now, bound), WorkerState::Done);
        assert_eq!(status_from(None, "dead", now, bound), WorkerState::Dead);
    }

    #[test]
    fn status_from_falls_back_to_screen_when_hook_state_is_stale() {
        let now = 2000;
        let bound = 600; // valid only if ts >= 1400

        let stale_working = HookState::new(WorkerState::Working, 1000); // 1000s old > 600s
        assert_eq!(
            status_from(Some(&stale_working), "idle", now, bound),
            WorkerState::Idle
        );

        let stale_blocked = HookState::new(WorkerState::Blocked, 1200); // 800s old > 600s
        assert_eq!(
            status_from(Some(&stale_blocked), "working", now, bound),
            WorkerState::Working
        );
    }

    #[test]
    fn worker_liveness_state_with_filesystem_and_screen_fallback() {
        let tmp = std::env::temp_dir().join(format!("quaqua-liveness-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let runs_dir = &tmp;
        let now = 1730000500;
        let bound = 600;

        // Screen says idle
        let f_idle = fake(
            Ok((
                true,
                r#"{"result":{"agent":{"name":"w1","agent_status":"idle"}}}"#.into(),
            )),
            Ok(LIST_W.into()),
        );

        // Case 1: No state file -> screen fallback returns Idle
        let st1 = worker_liveness_state(&f_idle, runs_dir, "w1", now, bound);
        assert_eq!(st1, WorkerState::Idle);

        // Case 2: State file says working -> hook state returns Working
        let state_file = runs_dir.join(".w1.state");
        std::fs::write(&state_file, format!("working {}\n", now - 10)).unwrap();
        let st2 = worker_liveness_state(&f_idle, runs_dir, "w1", now, bound);
        assert_eq!(st2, WorkerState::Working);

        // Case 3: State file says blocked -> hook state returns Blocked
        std::fs::write(&state_file, format!("blocked {}\n", now - 10)).unwrap();
        let st3 = worker_liveness_state(&f_idle, runs_dir, "w1", now, bound);
        assert_eq!(st3, WorkerState::Blocked);

        // Case 4: Stale state file (older than bound) -> falls back to screen
        std::fs::write(&state_file, format!("working {}\n", now - 1000)).unwrap();
        let st4 = worker_liveness_state(&f_idle, runs_dir, "w1", now, bound);
        assert_eq!(st4, WorkerState::Idle);

        // Case 5: Agent confirmed down -> Dead
        let f_down = fake(Ok((false, NOT_FOUND.into())), Ok(LIST_OTHER.into()));
        let st5 = worker_liveness_state(&f_down, runs_dir, "w1", now, bound);
        assert_eq!(st5, WorkerState::Dead);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
