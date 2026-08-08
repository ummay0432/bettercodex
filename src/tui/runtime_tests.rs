use super::MAX_READY_AGENT_EVENTS;
use super::drain_completed_agent_events;
use super::view::View;
use crate::assistant_message::AssistantMessage;
use crate::events::AgentEvent;
use codex_protocol::models::MessagePhase;
use std::path::Path;
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn completed_turn_drain_renders_events_beyond_the_fairness_batch() {
    const WIDTH: u16 = 80;
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("render every queued delta");
    let _ = view.take_pending_history_lines(WIDTH);
    let (events, mut ready) = unbounded_channel();
    let mut answer = "x".repeat(MAX_READY_AGENT_EVENTS);
    answer.push_str(" tail-marker");
    for _ in 0..MAX_READY_AGENT_EVENTS {
        events
            .send(AgentEvent::ModelMessageDelta("x".to_string()))
            .unwrap();
    }
    events
        .send(AgentEvent::ModelMessageDelta(" tail-marker".to_string()))
        .unwrap();
    events
        .send(AgentEvent::ModelMessageCompleted(AssistantMessage {
            text: answer,
            phase: Some(MessagePhase::FinalAnswer),
        }))
        .unwrap();
    drop(events);

    drain_completed_agent_events(&mut ready, |event| view.handle_agent_event(event));

    let rendered = view
        .take_pending_history_lines(WIDTH)
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(rendered.contains("tail-marker"), "{rendered}");
}
