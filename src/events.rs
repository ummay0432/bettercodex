use crate::assistant_message::AssistantMessage;
use crate::context::ContextSnapshot;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SteerId(pub(crate) u64);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelTextDelta {
    pub(crate) text: String,
    pub(crate) received_at: Instant,
}

impl ModelTextDelta {
    pub(crate) fn new(text: String, received_at: Instant) -> Self {
        Self { text, received_at }
    }

    #[cfg(test)]
    pub(crate) fn now(text: impl Into<String>) -> Self {
        Self::new(text.into(), Instant::now())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentEvent {
    ModelMessageStarted(AssistantMessage),
    ModelMessageDelta(ModelTextDelta),
    ReasoningSummarySectionStarted,
    ReasoningSummaryDelta(String),
    ModelMessageCompleted(AssistantMessage),
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
