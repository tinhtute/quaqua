//! Workflow abstractions and execution contracts for the event-sourced engine.
//!
//! Workflows are deterministic state machines executed step-by-step.
//! Inside [`Workflow::step`], workflows must not perform side-effects or direct I/O
//! (`std::fs`, `std::process`, `std::net`). All interaction with external systems
//! occurs through scheduled activities or timers.

use crate::engine::activity::{ActivityRequest, ActivityResult};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Pure workflow context passed to [`Workflow::step`].
///
/// An action scheduled during workflow execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScheduledAction {
    Activity {
        id: String,
        activity: ActivityRequest,
    },
    Timer {
        id: String,
        duration: Duration,
    },
    Child {
        id: String,
        workflow_type: String,
        input: serde_json::Value,
    },
}

/// Pure workflow context passed to [`Workflow::step`].
///
/// Exposes virtual scheduling capabilities and virtual time (`now()`) only.
/// Intentionally provides no handles to filesystem, processes, or network resources.
#[derive(Debug, Clone)]
pub struct WorkflowContext {
    now_ms: u64,
    scheduled: Vec<ScheduledAction>,
}

impl WorkflowContext {
    /// Creates a new workflow context with the specified virtual timestamp (milliseconds since Unix epoch).
    pub fn new(now_ms: u64) -> Self {
        Self {
            now_ms,
            scheduled: Vec::new(),
        }
    }

    /// Virtual current time in milliseconds since Unix epoch.
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// Virtual current time as [`SystemTime`].
    pub fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(self.now_ms)
    }

    /// Schedules an activity via the context.
    pub fn schedule_activity(&mut self, id: impl Into<String>, activity: ActivityRequest) {
        self.scheduled.push(ScheduledAction::Activity {
            id: id.into(),
            activity,
        });
    }

    /// Schedules a timer via the context.
    pub fn schedule_timer(&mut self, id: impl Into<String>, duration: Duration) {
        self.scheduled.push(ScheduledAction::Timer {
            id: id.into(),
            duration,
        });
    }

    /// Starts a child workflow via the context.
    pub fn start_child(
        &mut self,
        id: impl Into<String>,
        workflow_type: impl Into<String>,
        input: serde_json::Value,
    ) {
        self.scheduled.push(ScheduledAction::Child {
            id: id.into(),
            workflow_type: workflow_type.into(),
            input,
        });
    }

    /// Returns true if any actions have been scheduled in this context.
    pub fn has_scheduled(&self) -> bool {
        !self.scheduled.is_empty()
    }

    /// Returns a slice of the scheduled actions.
    pub fn scheduled(&self) -> &[ScheduledAction] {
        &self.scheduled
    }

    /// Drains and returns all scheduled actions from the context.
    pub fn drain_scheduled(&mut self) -> Vec<ScheduledAction> {
        std::mem::take(&mut self.scheduled)
    }
}

/// Incoming event delivered to a workflow step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WorkflowEvent {
    /// An activity previously scheduled has completed.
    ActivityCompleted { id: String, result: ActivityResult },
    /// A timer previously scheduled has fired.
    TimerFired { id: String },
    /// A child workflow has completed successfully.
    ChildCompleted {
        id: String,
        output: serde_json::Value,
    },
    /// A child workflow has failed.
    ChildFailed {
        id: String,
        code: i32,
        reason: String,
    },
    /// An approval pause was resolved.
    ApprovalResolved {
        approved: bool,
        reason: Option<String>,
    },
}

/// Outgoing decision returned by [`Workflow::step`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StepDecision<Output> {
    /// Transition state and continue immediately without external events.
    Continue,
    /// Schedule an asynchronous activity.
    ScheduleActivity {
        id: String,
        activity: ActivityRequest,
    },
    /// Schedule a timer for future execution.
    ScheduleTimer { id: String, duration: Duration },
    /// Start a child workflow.
    StartChild {
        id: String,
        workflow_type: String,
        input: serde_json::Value,
    },
    /// Pause workflow execution waiting for operator/user approval.
    PauseForApproval { reason: String },
    /// Complete the workflow with final output.
    Complete(Output),
    /// Terminate the workflow with failure.
    Fail { code: i32, reason: String },
}

/// Deterministic, event-driven workflow definition.
pub trait Workflow: Send + Sync + 'static {
    /// Initial input data for the workflow.
    type Input: Serialize + DeserializeOwned + Clone + Send + Sync + 'static;
    /// Workflow internal state, serializable for checkpointing/snapshots.
    type State: Serialize + DeserializeOwned + Clone + Send + Sync + 'static;
    /// Terminal workflow output.
    type Output: Serialize + DeserializeOwned + Clone + Send + Sync + 'static;

    /// Evaluates one deterministic step of the workflow.
    ///
    /// The workflow updates its `state` based on the incoming `event` and returns
    /// a [`StepDecision`].
    fn step(
        &self,
        state: &mut Self::State,
        ctx: &mut WorkflowContext,
        event: Option<WorkflowEvent>,
    ) -> StepDecision<Self::Output>;
}
