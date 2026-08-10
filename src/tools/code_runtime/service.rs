use std::sync::Arc;
use std::time::Duration;

use crate::tools::code_runtime::CellId;
use crate::tools::code_runtime::CodeModeNestedToolCall;
use crate::tools::code_runtime::CodeModeSessionDelegate;
use crate::tools::code_runtime::CodeModeSessionResultFuture;
use crate::tools::code_runtime::CodeModeToolKind;
use crate::tools::code_runtime::DEFAULT_CODE_MODE_EXEC_YIELD_TIME_MS;
use crate::tools::code_runtime::ExecuteRequest;
use crate::tools::code_runtime::FunctionCallOutputContentItem;
use crate::tools::code_runtime::ImageDetail;
#[cfg(test)]
use crate::tools::code_runtime::NoopCodeModeSessionDelegate;
use crate::tools::code_runtime::RuntimeResponse;
use crate::tools::code_runtime::StartedCell;
use crate::tools::code_runtime::WaitOutcome;
use crate::tools::code_runtime::WaitRequest;
use serde_json::Value as JsonValue;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::tools::code_runtime::session_runtime as runtime;
use crate::tools::code_runtime::session_runtime::SessionRuntime;

const YIELD_GRACE_PERIOD: Duration = Duration::from_secs(1);
const MIN_YIELD_TIME_FOR_GRACE: Duration = Duration::from_secs(10);

fn yield_timeout(yield_time_ms: u64) -> Duration {
    let yield_time = Duration::from_millis(yield_time_ms);
    if yield_time >= MIN_YIELD_TIME_FOR_GRACE {
        yield_time.saturating_add(YIELD_GRACE_PERIOD)
    } else {
        yield_time
    }
}

pub struct InProcessCodeModeSession {
    runtime: SessionRuntime<ProtocolDelegate>,
}

impl InProcessCodeModeSession {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_delegate(Arc::new(NoopCodeModeSessionDelegate))
    }

    pub fn with_delegate(delegate: Arc<dyn CodeModeSessionDelegate>) -> Self {
        Self {
            runtime: SessionRuntime::new(Arc::new(ProtocolDelegate { delegate })),
        }
    }

    pub async fn execute(&self, request: ExecuteRequest) -> Result<StartedCell, String> {
        let yield_time_ms = request
            .yield_time_ms
            .unwrap_or(DEFAULT_CODE_MODE_EXEC_YIELD_TIME_MS);
        let started = self
            .runtime
            .execute(runtime_request(request), yield_timeout(yield_time_ms))
            .await
            .map_err(|error| error.to_string())?;
        let cell_id = protocol_cell_id(&started.cell_id);
        let response_cell_id = cell_id.clone();
        let (response_tx, response_rx) = oneshot::channel();
        tokio::spawn(async move {
            let response = started
                .initial_event()
                .await
                .map_err(|error| error.to_string())
                .and_then(|event| runtime_response(&response_cell_id, event));
            let _ = response_tx.send(response);
        });
        Ok(StartedCell::from_result_receiver(cell_id, response_rx))
    }

    pub async fn wait(&self, request: WaitRequest) -> Result<WaitOutcome, String> {
        self.begin_wait(request).await.await
    }

    async fn begin_wait(
        &self,
        request: WaitRequest,
    ) -> CodeModeSessionResultFuture<'static, WaitOutcome> {
        let WaitRequest {
            cell_id,
            yield_time_ms,
        } = request;
        let runtime_cell_id = runtime_cell_id(&cell_id);
        match self
            .runtime
            .begin_observe(&runtime_cell_id, yield_timeout(yield_time_ms))
            .await
        {
            Ok(pending_event) => Box::pin(async move {
                match pending_event.event().await {
                    Ok(event) => Ok(WaitOutcome::LiveCell(runtime_response(&cell_id, event)?)),
                    Err(runtime::Error::MissingCell(_) | runtime::Error::ClosedCell(_)) => {
                        Ok(WaitOutcome::MissingCell(missing_cell_response(cell_id)))
                    }
                    Err(error) => Err(error.to_string()),
                }
            }),
            Err(runtime::Error::MissingCell(_) | runtime::Error::ClosedCell(_)) => {
                missing_wait(cell_id)
            }
            Err(error) => Box::pin(async move { Err(error.to_string()) }),
        }
    }

    pub async fn terminate(&self, cell_id: CellId) -> Result<WaitOutcome, String> {
        match self.runtime.terminate(&runtime_cell_id(&cell_id)).await {
            Ok(event) => Ok(WaitOutcome::LiveCell(runtime_response(&cell_id, event)?)),
            Err(runtime::Error::MissingCell(_) | runtime::Error::ClosedCell(_)) => {
                Ok(WaitOutcome::MissingCell(missing_cell_response(cell_id)))
            }
            Err(error) => Err(error.to_string()),
        }
    }

    #[cfg(test)]
    pub async fn shutdown(&self) -> Result<(), String> {
        self.runtime
            .shutdown()
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
impl Default for InProcessCodeModeSession {
    fn default() -> Self {
        Self::new()
    }
}

struct ProtocolDelegate {
    delegate: Arc<dyn CodeModeSessionDelegate>,
}

impl runtime::SessionRuntimeDelegate for ProtocolDelegate {
    async fn invoke_tool(
        &self,
        invocation: runtime::NestedToolCall,
        cancellation_token: CancellationToken,
    ) -> Result<JsonValue, String> {
        self.delegate
            .invoke_tool(
                CodeModeNestedToolCall {
                    cell_id: protocol_cell_id(&invocation.cell_id),
                    runtime_tool_call_id: invocation.runtime_tool_call_id,
                    tool_name: crate::protocol::ToolName {
                        name: invocation.tool_name.name,
                        namespace: invocation.tool_name.namespace,
                    },
                    tool_kind: match invocation.tool_kind {
                        runtime::ToolKind::Function => CodeModeToolKind::Function,
                        runtime::ToolKind::Freeform => CodeModeToolKind::Freeform,
                    },
                    input: invocation.input,
                },
                cancellation_token,
            )
            .await
    }

    async fn notify(
        &self,
        call_id: String,
        cell_id: runtime::CellId,
        text: String,
        cancellation_token: CancellationToken,
    ) -> Result<(), String> {
        self.delegate
            .notify(
                call_id,
                protocol_cell_id(&cell_id),
                text,
                cancellation_token,
            )
            .await
    }

    fn cell_closed(&self, cell_id: &runtime::CellId) {
        self.delegate.cell_closed(&protocol_cell_id(cell_id));
    }
}

fn runtime_request(request: ExecuteRequest) -> runtime::CreateCellRequest {
    runtime::CreateCellRequest {
        tool_call_id: request.tool_call_id,
        enabled_tools: request
            .enabled_tools
            .into_iter()
            .map(|definition| runtime::ToolDefinition {
                name: definition.name,
                tool_name: runtime::ToolName {
                    name: definition.tool_name.name,
                    namespace: definition.tool_name.namespace,
                },
                description: definition.description,
                kind: match definition.kind {
                    CodeModeToolKind::Function => runtime::ToolKind::Function,
                    CodeModeToolKind::Freeform => runtime::ToolKind::Freeform,
                },
            })
            .collect(),
        source: request.source,
    }
}

fn runtime_cell_id(cell_id: &CellId) -> runtime::CellId {
    runtime::CellId::new(cell_id.as_str())
}

fn protocol_cell_id(cell_id: &runtime::CellId) -> CellId {
    CellId::new(cell_id.as_str().to_string())
}

fn runtime_response(
    cell_id: &CellId,
    event: runtime::CellEvent,
) -> Result<RuntimeResponse, String> {
    match event {
        runtime::CellEvent::Yielded { content_items } => Ok(RuntimeResponse::Yielded {
            cell_id: cell_id.clone(),
            content_items: content_items.into_iter().map(output_item).collect(),
        }),
        runtime::CellEvent::Completed {
            content_items,
            error_text,
        } => Ok(RuntimeResponse::Result {
            cell_id: cell_id.clone(),
            content_items: content_items.into_iter().map(output_item).collect(),
            error_text,
        }),
        runtime::CellEvent::Terminated { content_items } => Ok(RuntimeResponse::Terminated {
            cell_id: cell_id.clone(),
            content_items: content_items.into_iter().map(output_item).collect(),
        }),
    }
}

fn output_item(item: runtime::OutputItem) -> FunctionCallOutputContentItem {
    match item {
        runtime::OutputItem::Text { text } => FunctionCallOutputContentItem::InputText { text },
        runtime::OutputItem::Image { image_url, detail } => {
            FunctionCallOutputContentItem::InputImage {
                image_url,
                detail: detail.map(|detail| match detail {
                    runtime::ImageDetail::Auto => ImageDetail::Auto,
                    runtime::ImageDetail::Low => ImageDetail::Low,
                    runtime::ImageDetail::High => ImageDetail::High,
                    runtime::ImageDetail::Original => ImageDetail::Original,
                }),
            }
        }
    }
}

fn missing_cell_response(cell_id: CellId) -> RuntimeResponse {
    RuntimeResponse::Result {
        error_text: Some(format!("exec cell {cell_id} not found")),
        cell_id,
        content_items: Vec::new(),
    }
}

fn missing_wait(cell_id: CellId) -> CodeModeSessionResultFuture<'static, WaitOutcome> {
    Box::pin(async move { Ok(WaitOutcome::MissingCell(missing_cell_response(cell_id))) })
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "service_contract_tests.rs"]
mod contract_tests;
