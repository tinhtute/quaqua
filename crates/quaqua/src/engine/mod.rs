//! The Quaqua embedded workflow engine core (W0 contract).
//!
//! Exposes event-sourced workflow interfaces, activity execution contracts,
//! object-safe journaling, GenAI telemetry, child worker definitions, and
//! deterministic in-memory testkit.

pub mod activities;
pub mod activity;
pub mod child;
pub mod clock;
pub mod guard;
pub mod journal;
pub mod journal_sqlite;
pub mod runner;
pub mod sqlite;
pub mod telemetry;
pub mod telemetry_otel;
pub mod testkit;
pub mod workflow;

#[cfg(test)]
mod tests;

pub use activity::{Activity, ActivityError, ActivityRequest, ActivityResult};
pub use child::{SingleWorkerInput, SingleWorkerOutput};
pub use guard::{
    scan_directory_for_determinism_violations, scan_source_for_determinism_violations,
    DeterminismViolation,
};
pub use journal::{EventRecord, Journal, JournalError, SnapshotRecord};
pub use telemetry::{
    GenAiSpan, SpanSink, TelemetryError, GEN_AI_AGENT_NAME, GEN_AI_AGENT_ROLE,
    GEN_AI_OPERATION_NAME, GEN_AI_REQUEST_MODEL, GEN_AI_SYSTEM, GEN_AI_USAGE_INPUT_TOKENS,
    GEN_AI_USAGE_OUTPUT_TOKENS,
};
pub use testkit::{run_to_completion, InMemoryJournal, MockActivities, TestkitError, VirtualClock};
pub use workflow::{ScheduledAction, StepDecision, Workflow, WorkflowContext, WorkflowEvent};
