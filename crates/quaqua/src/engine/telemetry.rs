//! Telemetry traits and OpenTelemetry GenAI semantic conventions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ----------------------------------------------------------------------------
// OpenTelemetry GenAI Semantic Attribute Names
// ----------------------------------------------------------------------------

/// The generative AI system in use (e.g. "openai", "anthropic", "custom").
pub const GEN_AI_SYSTEM: &str = "gen_ai.system";

/// The model requested for generation (e.g. "claude-3-5-sonnet", "deepseek-v4.1").
pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";

/// Number of input (prompt) tokens consumed.
pub const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";

/// Number of output (completion) tokens generated.
pub const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";

/// High-level GenAI operation name (e.g. "chat", "text_completion", "agent_step").
pub const GEN_AI_OPERATION_NAME: &str = "gen_ai.operation.name";

/// The unique identifier or name of the agent executing the step.
pub const GEN_AI_AGENT_NAME: &str = "gen_ai.agent.name";

/// The role/profile of the agent (e.g. "cos", "lead", "builder", "reviewer").
pub const GEN_AI_AGENT_ROLE: &str = "gen_ai.agent.role";

/// Error type for telemetry operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TelemetryError {
    SinkError(String),
}

impl std::fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SinkError(msg) => write!(f, "telemetry sink error: {msg}"),
        }
    }
}

impl std::error::Error for TelemetryError {}

/// Typed span structure for GenAI agent execution steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenAiSpan {
    pub trace_id: String,
    pub span_id: String,
    pub operation_name: String,
    pub system: String,
    pub request_model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub agent_name: Option<String>,
    pub agent_role: Option<String>,
    pub start_time_ms: u64,
    pub end_time_ms: u64,
    pub attributes: BTreeMap<String, String>,
}

impl GenAiSpan {
    /// Creates a span populated with standard GenAI attributes.
    pub fn new(
        trace_id: impl Into<String>,
        span_id: impl Into<String>,
        operation_name: impl Into<String>,
        system: impl Into<String>,
        start_time_ms: u64,
        end_time_ms: u64,
    ) -> Self {
        let mut attributes = BTreeMap::new();
        let sys = system.into();
        let op = operation_name.into();
        attributes.insert(GEN_AI_SYSTEM.to_string(), sys.clone());
        attributes.insert(GEN_AI_OPERATION_NAME.to_string(), op.clone());

        Self {
            trace_id: trace_id.into(),
            span_id: span_id.into(),
            operation_name: op,
            system: sys,
            request_model: None,
            input_tokens: None,
            output_tokens: None,
            agent_name: None,
            agent_role: None,
            start_time_ms,
            end_time_ms,
            attributes,
        }
    }

    /// Sets model and updates attributes.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let m = model.into();
        self.attributes
            .insert(GEN_AI_REQUEST_MODEL.to_string(), m.clone());
        self.request_model = Some(m);
        self
    }

    /// Sets token usage and updates attributes.
    pub fn with_usage(mut self, input_tokens: u64, output_tokens: u64) -> Self {
        self.attributes.insert(
            GEN_AI_USAGE_INPUT_TOKENS.to_string(),
            input_tokens.to_string(),
        );
        self.attributes.insert(
            GEN_AI_USAGE_OUTPUT_TOKENS.to_string(),
            output_tokens.to_string(),
        );
        self.input_tokens = Some(input_tokens);
        self.output_tokens = Some(output_tokens);
        self
    }

    /// Sets agent identity and updates attributes.
    pub fn with_agent(mut self, name: impl Into<String>, role: impl Into<String>) -> Self {
        let n = name.into();
        let r = role.into();
        self.attributes
            .insert(GEN_AI_AGENT_NAME.to_string(), n.clone());
        self.attributes
            .insert(GEN_AI_AGENT_ROLE.to_string(), r.clone());
        self.agent_name = Some(n);
        self.agent_role = Some(r);
        self
    }
}

/// Sink for emitting GenAI execution spans.
pub trait SpanSink: Send + Sync {
    /// Records a completed GenAI span.
    fn record_span(&self, span: &GenAiSpan) -> Result<(), TelemetryError>;
}
