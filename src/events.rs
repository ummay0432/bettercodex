use crate::context::ContextSnapshot;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SteerId(pub(crate) u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentEvent {
    ModelTextDelta(String),
    ReasoningSummarySectionStarted,
    ReasoningSummaryDelta(String),
    ModelItemCompleted,
    ModelResponseCompleted,
    ToolStarted {
        call_id: String,
        name: String,
        input: Option<Value>,
    },
    ToolCompleted {
        call_id: String,
        output: Result<Value, String>,
        duration: Duration,
    },
    ContextUpdated(ContextSnapshot),
    Warning(String),
    SteeringCommitted(SteerId),
    CompactionStarted,
    CompactionCompleted,
}
