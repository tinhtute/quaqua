//! Object-safe journal interface for workflow event sourcing.
//!
//! # SQLite Schema
//!
//! The production journal implementation (landing in W1 under `journal_sqlite.rs`)
//! uses SQLite WAL mode at `<workspace_root>/runs/quaqua.db` with the following schema:
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS events (
//!     workflow_id TEXT NOT NULL,
//!     seq INTEGER NOT NULL,
//!     ts_ms INTEGER NOT NULL,
//!     kind TEXT NOT NULL,
//!     payload_json TEXT NOT NULL,
//!     PRIMARY KEY (workflow_id, seq)
//! );
//!
//! CREATE TABLE IF NOT EXISTS snapshots (
//!     workflow_id TEXT NOT NULL,
//!     seq INTEGER NOT NULL,
//!     state_json TEXT NOT NULL,
//!     PRIMARY KEY (workflow_id, seq)
//! );
//! ```

use serde::{Deserialize, Serialize};

/// Error type for journal operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JournalError {
    /// Database read/write error.
    StorageError(String),
    /// Serialization or deserialization error.
    SerializationError(String),
    /// Sequence conflict during append.
    SequenceConflict {
        workflow_id: String,
        expected_seq: u64,
        actual_seq: u64,
    },
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StorageError(msg) => write!(f, "journal storage error: {msg}"),
            Self::SerializationError(msg) => write!(f, "journal serialization error: {msg}"),
            Self::SequenceConflict {
                workflow_id,
                expected_seq,
                actual_seq,
            } => {
                write!(f, "journal sequence conflict for {workflow_id}: expected {expected_seq}, got {actual_seq}")
            }
        }
    }
}

impl std::error::Error for JournalError {}

/// An event entry recorded in the journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    /// Monotonically increasing sequence number per workflow.
    pub seq: u64,
    /// Timestamp in milliseconds since Unix epoch.
    pub ts_ms: u64,
    /// Event kind tag (e.g. "activity_completed", "timer_fired").
    pub kind: String,
    /// JSON payload representing event details.
    pub payload_json: String,
}

impl EventRecord {
    pub fn new(
        seq: u64,
        ts_ms: u64,
        kind: impl Into<String>,
        payload_json: impl Into<String>,
    ) -> Self {
        Self {
            seq,
            ts_ms,
            kind: kind.into(),
            payload_json: payload_json.into(),
        }
    }
}

/// A workflow state snapshot recorded in the journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRecord {
    /// Sequence number up to which events have been incorporated into this snapshot.
    pub seq: u64,
    /// Serialized JSON string of workflow state.
    pub state_json: String,
}

impl SnapshotRecord {
    pub fn new(seq: u64, state_json: impl Into<String>) -> Self {
        Self {
            seq,
            state_json: state_json.into(),
        }
    }
}

/// Object-safe trait for workflow event journaling and state snapshotting.
pub trait Journal: Send + Sync {
    /// Appends an event to the workflow history and returns the assigned sequence number.
    fn append_event(&mut self, workflow_id: &str, event: &EventRecord)
        -> Result<u64, JournalError>;

    /// Appends a workflow state snapshot.
    fn append_snapshot(
        &mut self,
        workflow_id: &str,
        snapshot: &SnapshotRecord,
    ) -> Result<(), JournalError>;

    /// Loads historical events for a workflow starting from sequence `since_seq` (inclusive).
    fn load_events(
        &self,
        workflow_id: &str,
        since_seq: u64,
    ) -> Result<Vec<EventRecord>, JournalError>;

    /// Loads the latest state snapshot for a workflow, if one exists.
    fn load_latest_snapshot(
        &self,
        workflow_id: &str,
    ) -> Result<Option<SnapshotRecord>, JournalError>;
}
