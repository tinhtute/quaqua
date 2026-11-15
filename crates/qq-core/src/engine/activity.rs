//! Activity trait and typed request/result contracts.
//!
//! Activities encapsulate all side-effecting operations (herdr execution,
//! git commands, pane grid layouts, report filesystem access, and cost tracking).

use serde::{Deserialize, Serialize};

/// Common error type for activity execution failures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ActivityError {
    /// The activity execution failed with an error message.
    ExecutionFailed(String),
    /// The target resource was not found.
    NotFound(String),
    /// The operation timed out.
    Timeout(String),
    /// The operation was cancelled.
    Cancelled(String),
}

impl std::fmt::Display for ActivityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExecutionFailed(msg) => write!(f, "activity execution failed: {msg}"),
            Self::NotFound(msg) => write!(f, "activity resource not found: {msg}"),
            Self::Timeout(msg) => write!(f, "activity timed out: {msg}"),
            Self::Cancelled(msg) => write!(f, "activity cancelled: {msg}"),
        }
    }
}

impl std::error::Error for ActivityError {}

// ----------------------------------------------------------------------------
// Herdr Control
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum HerdrRequest {
    Spawn {
        name: String,
        kind: String,
        repo: String,
        #[serde(default)]
        approve_kinds: Vec<String>,
    },
    Cleanup {
        name: String,
        repo: String,
    },
    Prompt {
        name: String,
        task: String,
    },
    SendKeys {
        name: String,
        keys: String,
    },
    Stop {
        name: String,
    },
    /// One blocking native wait (or a plain `agent get` when `timeout_ms` is 0).
    AgentWait {
        name: String,
        #[serde(default)]
        until: Vec<String>,
        timeout_ms: u64,
    },
    /// Deserialize-only tombstone for removed variants.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum HerdrResult {
    Spawned {
        name: String,
    },
    Cleaned {
        name: String,
    },
    Prompted {
        name: String,
    },
    KeysSent {
        name: String,
    },
    Stopped {
        name: String,
    },
    /// Journal tombstone: old runs may carry a pane read result.
    AgentOutput {
        content: String,
    },
    AgentWait {
        name: String,
        status: String,
        seq: u64,
        session_kind: String,
        session_value: String,
        harness: String,
        cwd: String,
        /// `agent_not_running`, `agent_not_found`, an io/socket error, or empty.
        err: String,
        /// True when this agent is the Lead of a live pipeline.
        lead_waiting: bool,
        /// herdr's screen-detection flag (`screen_detection_skipped`).
        #[serde(default)]
        hook_active: bool,
    },
    /// Deserialize-only tombstone for removed result variants.
    Unknown,
}

// ----------------------------------------------------------------------------
// Harness session store
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SessionRequest {
    /// Last write timestamp of the harness session store for one session.
    Mtime {
        harness: String,
        session_kind: String,
        session_value: String,
        cwd: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SessionResult {
    /// Milliseconds since the epoch, or `None` when the store is unreadable.
    Mtime(Option<u64>),
}

// ----------------------------------------------------------------------------
// Git
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GitRequest {
    WorktreeAdd {
        path: String,
        branch: String,
        base: Option<String>,
    },
    WorktreeRemove {
        path: String,
    },
    BranchCreate {
        name: String,
        base: String,
    },
    Commit {
        message: String,
    },
    Merge {
        branch: String,
    },
    CurrentSha,
    DiffHash {
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GitResult {
    WorktreeAdded { path: String },
    WorktreeRemoved { path: String },
    BranchCreated { name: String },
    Committed { sha: String },
    Merged { sha: String },
    Sha(String),
    DiffHash { hash: String, stat: String },
}

// ----------------------------------------------------------------------------
// Grid
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GridRequest {
    Reconcile {
        tab: Option<String>,
        only: Option<String>,
    },
    Plan {
        tab: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GridResult {
    Reconciled { changed: bool },
    PlanJson(String),
}

// ----------------------------------------------------------------------------
// Report File
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReportFileRequest {
    Read { path: String },
    Write { path: String, content: String },
    CheckMtime { path: String },
    Exists { path: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReportFileResult {
    Content(String),
    Written,
    MtimeMs(u64),
    Exists(bool),
}

// ----------------------------------------------------------------------------
// Cost
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CostRequest {
    QuerySpend { session_pattern: String },
    CheckBudget { spend: f64, budget: f64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CostResult {
    Spend(f64),
    BudgetExceeded(bool),
}

// ----------------------------------------------------------------------------
// Unified Request / Result Enums
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ActivityRequest {
    Herdr(HerdrRequest),
    Git(GitRequest),
    Grid(GridRequest),
    ReportFile(ReportFileRequest),
    Cost(CostRequest),
    Session(SessionRequest),
    /// Deserialize-only tombstone for removed activity families.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ActivityResult {
    Herdr(HerdrResult),
    Git(GitResult),
    Grid(GridResult),
    ReportFile(ReportFileResult),
    Cost(CostResult),
    Session(SessionResult),
    /// Deserialize-only tombstone for removed activity families.
    Unknown,
}

// Journals written before the event-driven watchdog contain removed variants
// (events, pane reads, agent info). Decode those as tombstones instead
// of failing the whole replay: a known tag with an unknown shape is a no-op.
fn tag_value(value: serde_json::Value) -> Option<(String, serde_json::Value)> {
    let obj = value.as_object()?;
    let (tag, inner) = obj.iter().next()?;
    Some((tag.clone(), inner.clone()))
}

#[derive(Deserialize)]
struct SpawnReqPayload {
    name: String,
    kind: String,
    repo: String,
    #[serde(default)]
    approve_kinds: Vec<String>,
}
#[derive(Deserialize)]
struct CleanupReqPayload {
    name: String,
    repo: String,
}
#[derive(Deserialize)]
struct PromptReqPayload {
    name: String,
    task: String,
}
#[derive(Deserialize)]
struct SendKeysReqPayload {
    name: String,
    keys: String,
}
#[derive(Deserialize)]
struct StopReqPayload {
    name: String,
}
#[derive(Deserialize)]
struct AgentWaitReqPayload {
    name: String,
    #[serde(default)]
    until: Vec<String>,
    timeout_ms: u64,
}

impl<'de> Deserialize<'de> for HerdrRequest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        let Some((tag, inner)) = tag_value(value) else {
            return Ok(HerdrRequest::Unknown);
        };
        Ok(match tag.as_str() {
            "Spawn" => match serde_json::from_value::<SpawnReqPayload>(inner) {
                Ok(p) => HerdrRequest::Spawn {
                    name: p.name,
                    kind: p.kind,
                    repo: p.repo,
                    approve_kinds: p.approve_kinds,
                },
                Err(_) => HerdrRequest::Unknown,
            },
            "Cleanup" => match serde_json::from_value::<CleanupReqPayload>(inner) {
                Ok(p) => HerdrRequest::Cleanup {
                    name: p.name,
                    repo: p.repo,
                },
                Err(_) => HerdrRequest::Unknown,
            },
            "Prompt" => match serde_json::from_value::<PromptReqPayload>(inner) {
                Ok(p) => HerdrRequest::Prompt {
                    name: p.name,
                    task: p.task,
                },
                Err(_) => HerdrRequest::Unknown,
            },
            "SendKeys" => match serde_json::from_value::<SendKeysReqPayload>(inner) {
                Ok(p) => HerdrRequest::SendKeys {
                    name: p.name,
                    keys: p.keys,
                },
                Err(_) => HerdrRequest::Unknown,
            },
            "Stop" => match serde_json::from_value::<StopReqPayload>(inner) {
                Ok(p) => HerdrRequest::Stop { name: p.name },
                Err(_) => HerdrRequest::Unknown,
            },
            "AgentWait" => match serde_json::from_value::<AgentWaitReqPayload>(inner) {
                Ok(p) => HerdrRequest::AgentWait {
                    name: p.name,
                    until: p.until,
                    timeout_ms: p.timeout_ms,
                },
                Err(_) => HerdrRequest::Unknown,
            },
            _ => HerdrRequest::Unknown,
        })
    }
}

#[derive(Deserialize)]
struct SpawnedResultPayload {
    name: String,
}
#[derive(Deserialize)]
struct CleanedResultPayload {
    name: String,
}
#[derive(Deserialize)]
struct PromptedResultPayload {
    name: String,
}
#[derive(Deserialize)]
struct KeysSentResultPayload {
    name: String,
}
#[derive(Deserialize)]
struct StoppedResultPayload {
    name: String,
}
#[derive(Deserialize)]
struct AgentOutputResultPayload {
    content: String,
}
#[derive(Deserialize)]
struct AgentWaitResultPayload {
    name: String,
    status: String,
    seq: u64,
    session_kind: String,
    session_value: String,
    harness: String,
    cwd: String,
    #[serde(default)]
    err: String,
    #[serde(default)]
    lead_waiting: bool,
    #[serde(default)]
    hook_active: bool,
}

impl<'de> Deserialize<'de> for HerdrResult {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        let Some((tag, inner)) = tag_value(value) else {
            return Ok(HerdrResult::Unknown);
        };
        Ok(match tag.as_str() {
            "Spawned" => match serde_json::from_value::<SpawnedResultPayload>(inner) {
                Ok(p) => HerdrResult::Spawned { name: p.name },
                Err(_) => HerdrResult::Unknown,
            },
            "Cleaned" => match serde_json::from_value::<CleanedResultPayload>(inner) {
                Ok(p) => HerdrResult::Cleaned { name: p.name },
                Err(_) => HerdrResult::Unknown,
            },
            "Prompted" => match serde_json::from_value::<PromptedResultPayload>(inner) {
                Ok(p) => HerdrResult::Prompted { name: p.name },
                Err(_) => HerdrResult::Unknown,
            },
            "KeysSent" => match serde_json::from_value::<KeysSentResultPayload>(inner) {
                Ok(p) => HerdrResult::KeysSent { name: p.name },
                Err(_) => HerdrResult::Unknown,
            },
            "Stopped" => match serde_json::from_value::<StoppedResultPayload>(inner) {
                Ok(p) => HerdrResult::Stopped { name: p.name },
                Err(_) => HerdrResult::Unknown,
            },
            "AgentOutput" => match serde_json::from_value::<AgentOutputResultPayload>(inner) {
                Ok(p) => HerdrResult::AgentOutput { content: p.content },
                Err(_) => HerdrResult::Unknown,
            },
            "AgentWait" => match serde_json::from_value::<AgentWaitResultPayload>(inner) {
                Ok(p) => HerdrResult::AgentWait {
                    name: p.name,
                    status: p.status,
                    seq: p.seq,
                    session_kind: p.session_kind,
                    session_value: p.session_value,
                    harness: p.harness,
                    cwd: p.cwd,
                    err: p.err,
                    lead_waiting: p.lead_waiting,
                    hook_active: p.hook_active,
                },
                Err(_) => HerdrResult::Unknown,
            },
            _ => HerdrResult::Unknown,
        })
    }
}

impl<'de> Deserialize<'de> for ActivityRequest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        let Some((tag, inner)) = tag_value(value) else {
            return Ok(ActivityRequest::Unknown);
        };
        Ok(match tag.as_str() {
            "Herdr" => ActivityRequest::Herdr(
                serde_json::from_value(inner).unwrap_or(HerdrRequest::Unknown),
            ),
            "Git" => match serde_json::from_value(inner) {
                Ok(v) => ActivityRequest::Git(v),
                Err(_) => ActivityRequest::Unknown,
            },
            "Grid" => match serde_json::from_value(inner) {
                Ok(v) => ActivityRequest::Grid(v),
                Err(_) => ActivityRequest::Unknown,
            },
            "ReportFile" => match serde_json::from_value(inner) {
                Ok(v) => ActivityRequest::ReportFile(v),
                Err(_) => ActivityRequest::Unknown,
            },
            "Cost" => match serde_json::from_value(inner) {
                Ok(v) => ActivityRequest::Cost(v),
                Err(_) => ActivityRequest::Unknown,
            },
            "Session" => match serde_json::from_value(inner) {
                Ok(v) => ActivityRequest::Session(v),
                Err(_) => ActivityRequest::Unknown,
            },
            _ => ActivityRequest::Unknown,
        })
    }
}

impl<'de> Deserialize<'de> for ActivityResult {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        let Some((tag, inner)) = tag_value(value) else {
            return Ok(ActivityResult::Unknown);
        };
        Ok(match tag.as_str() {
            "Herdr" => {
                ActivityResult::Herdr(serde_json::from_value(inner).unwrap_or(HerdrResult::Unknown))
            }
            "Git" => match serde_json::from_value(inner) {
                Ok(v) => ActivityResult::Git(v),
                Err(_) => ActivityResult::Unknown,
            },
            "Grid" => match serde_json::from_value(inner) {
                Ok(v) => ActivityResult::Grid(v),
                Err(_) => ActivityResult::Unknown,
            },
            "ReportFile" => match serde_json::from_value(inner) {
                Ok(v) => ActivityResult::ReportFile(v),
                Err(_) => ActivityResult::Unknown,
            },
            "Cost" => match serde_json::from_value(inner) {
                Ok(v) => ActivityResult::Cost(v),
                Err(_) => ActivityResult::Unknown,
            },
            "Session" => match serde_json::from_value(inner) {
                Ok(v) => ActivityResult::Session(v),
                Err(_) => ActivityResult::Unknown,
            },
            _ => ActivityResult::Unknown,
        })
    }
}

/// Object-safe activity execution interface.
pub trait Activity: Send + Sync {
    /// Executes a typed activity request and returns a typed result.
    fn execute(&self, req: &ActivityRequest) -> Result<ActivityResult, ActivityError>;
}

#[cfg(test)]
mod roundtrip_tests {
    use super::*;

    #[test]
    fn cost_result_roundtrips_through_value() {
        let r = ActivityResult::Cost(CostResult::Spend(10.0));
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(serde_json::from_value::<ActivityResult>(v).unwrap(), r);
    }

    #[test]
    fn agent_wait_result_roundtrips_through_value() {
        let r = ActivityResult::Herdr(HerdrResult::AgentWait {
            name: "sude-test".to_string(),
            status: "working".to_string(),
            seq: 1,
            session_kind: "path".to_string(),
            session_value: "/tmp/sess".to_string(),
            harness: "omp".to_string(),
            cwd: "/tmp".to_string(),
            err: String::new(),
            lead_waiting: false,
            hook_active: false,
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(serde_json::from_value::<ActivityResult>(v).unwrap(), r);
    }
}
