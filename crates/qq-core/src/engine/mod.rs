//! The qq-core workflow engine: event-sourced workflows replayed from a journal.
//!
//! A workflow step is pure. Effects are activities the runner executes and
//! journals; observations pushed from outside arrive as `WorkflowEvent::Signal`.
//! The clock seam (`next_deadline` / `fire_due`) never sleeps, so one driver
//! thread can wait on an event channel and the next deadline at once.

pub mod activity;
pub mod clock;
pub mod guard;
pub mod journal;
pub mod journal_sqlite;
pub mod mocks;
pub mod runner;
pub mod sqlite;
pub mod workflow;

pub use activity::{Activity, ActivityError, ActivityRequest, ActivityResult};
pub use guard::{
    scan_directory_for_determinism_violations, scan_source_for_determinism_violations,
    DeterminismViolation,
};
pub use journal::{EventRecord, Journal, JournalError, SnapshotRecord};
pub use mocks::{InMemoryJournal, MockActivities, VirtualClock};
pub use workflow::{ScheduledAction, StepDecision, Workflow, WorkflowContext, WorkflowEvent};
