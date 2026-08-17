use super::ApiError;
use super::ApiResult;
use futures_util::SinkExt;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio::time::timeout;
use tokio::time::timeout_at;
use tokio_tungstenite::Connector;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async_tls_with_config;
use tungstenite::Message;
use tungstenite::Utf8Bytes;
use tungstenite::client::IntoClientRequest;
use tungstenite::extensions::ExtensionsConfig;
use tungstenite::extensions::compression::deflate::DeflateConfig;
use tungstenite::protocol::WebSocketConfig;

type ResponsesWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum WebSocketCommand {
    Send {
        message: Message,
        result: oneshot::Sender<Result<(), tungstenite::Error>>,
    },
}

pub(super) struct WebSocketConnection {
    commands: mpsc::Sender<WebSocketCommand>,
    messages: mpsc::UnboundedReceiver<Result<Message, tungstenite::Error>>,
    pump: JoinHandle<()>,
}

impl WebSocketConnection {
    fn new(stream: ResponsesWebSocket) -> Self {
        let (commands, mut command_rx) = mpsc::channel::<WebSocketCommand>(32);
        let (message_tx, messages) = mpsc::unbounded_channel();
        let pump = tokio::spawn(async move {
            let mut stream = stream;
            loop {
                tokio::select! {
                    command = command_rx.recv() => {
                        let Some(command) = command else {
                            break;
                        };
                        match command {
                            WebSocketCommand::Send { message, result } => {
                                let send_result = stream.send(message).await;
                                let should_stop = send_result.is_err();
                                let _ = result.send(send_result);
                                if should_stop {
                                    break;
                                }
                            }
                        }
                    }
                    message = stream.next() => {
                        let Some(message) = message else {
                            break;
                        };
                        match message {
                            Ok(Message::Ping(payload)) => {
                                if let Err(error) = stream.send(Message::Pong(payload)).await {
                                    let _ = message_tx.send(Err(error));
                                    break;
                                }
                            }
                            Ok(Message::Pong(_)) => {}
                            Ok(message @ (Message::Text(_)
                                | Message::Binary(_)
                                | Message::Close(_)
                                | Message::Frame(_))) => {
                                let is_close = matches!(message, Message::Close(_));
                                if message_tx.send(Ok(message)).is_err() || is_close {
                                    break;
                                }
                            }
                            Err(error) => {
                                let _ = message_tx.send(Err(error));
                                break;
                            }
                        }
                    }
                }
            }
        });
        Self {
            commands,
            messages,
            pump,
        }
    }

    pub(super) async fn connect(
        url: &str,
        headers: &HeaderMap,
        tls_config: Option<Arc<rustls::ClientConfig>>,
    ) -> ApiResult<(Self, HeaderMap)> {
        let mut request = url.into_client_request().map_err(|error| {
            ApiError::fatal(format!("failed to build WebSocket request: {error}"))
        })?;
        request.headers_mut().extend(headers.clone());
        let connector = if request.uri().scheme_str() == Some("wss") {
            tls_config.map(Connector::Rustls)
        } else {
            None
        };
        let connect =
            connect_async_tls_with_config(request, Some(websocket_config()), false, connector);
        let (stream, response) = timeout(super::WEBSOCKET_CONNECT_TIMEOUT, connect)
            .await
            .map_err(|_| ApiError::retryable("timed out connecting Responses WebSocket"))?
            .map_err(map_connect_error)?;
        Ok((Self::new(stream), response.headers().clone()))
    }

    pub(super) fn is_closed(&self) -> bool {
        self.pump.is_finished() || self.commands.is_closed()
    }

    pub(super) async fn send<T: Serialize + ?Sized>(
        &mut self,
        request: &T,
        idle_timeout: Duration,
    ) -> ApiResult<()> {
        let encoded = serde_json::to_string(request).map_err(|error| {
            ApiError::fatal(format!("failed to encode WebSocket request: {error}"))
        })?;
        timeout(
            idle_timeout,
            self.send_message(Message::Text(encoded.into())),
        )
        .await
        .map_err(|_| ApiError::stream_idle("Responses WebSocket send was inactive for too long"))?
        .map_err(|error| ApiError::retryable(format!("failed to send WebSocket request: {error}")))
    }

    async fn send_message(&self, message: Message) -> Result<(), tungstenite::Error> {
        let (result, result_rx) = oneshot::channel();
        self.commands
            .send(WebSocketCommand::Send { message, result })
            .await
            .map_err(|_| tungstenite::Error::ConnectionClosed)?;
        result_rx
            .await
            .unwrap_or(Err(tungstenite::Error::ConnectionClosed))
    }

    pub(super) async fn next_text(&mut self, idle_timeout: Duration) -> ApiResult<Utf8Bytes> {
        let deadline = Instant::now() + idle_timeout;
        loop {
            let message = timeout_at(deadline, self.messages.recv())
                .await
                .map_err(|_| {
                    ApiError::stream_idle("Responses WebSocket was inactive for too long")
                })?;
            let Some(message) = message else {
                return Err(ApiError::retryable(
                    "WebSocket closed before response.completed",
                ));
            };
            match message {
                Ok(Message::Text(text)) => return Ok(text),
                Ok(Message::Close(frame)) => {
                    let reason = frame
                        .map(|frame| frame.reason.to_string())
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or_else(|| "server closed the connection".to_string());
                    return Err(ApiError::retryable(format!(
                        "WebSocket closed before response.completed: {reason}"
                    )));
                }
                Ok(Message::Binary(_)) => {
                    return Err(ApiError::fatal(
                        "Responses WebSocket sent an unexpected binary event",
                    ));
                }
                Ok(Message::Frame(_)) => {}
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                Err(error) => {
                    return Err(ApiError::retryable(format!(
                        "Responses WebSocket failed: {error}"
                    )));
                }
            }
        }
    }
}

impl Drop for WebSocketConnection {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

pub(super) fn websocket_config() -> WebSocketConfig {
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(DeflateConfig::default());

    let mut config = WebSocketConfig::default();
    config.extensions = extensions;
    config.max_message_size = Some(super::MAX_STREAM_EVENT_BYTES);
    config
}

fn map_connect_error(error: tungstenite::Error) -> ApiError {
    match error {
        tungstenite::Error::Http(response) => {
            let status = response.status();
            let headers = response.headers().clone();
            let body = response
                .body()
                .as_ref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_default();
            let message = format!("Responses WebSocket upgrade failed with {status}: {body}");
            let error = if status.as_u16() == 401 {
                ApiError::unauthorized(format!("Responses WebSocket authentication failed: {body}"))
            } else if matches!(status.as_u16(), 404 | 405 | 426) {
                ApiError::websocket_unavailable(message)
            } else if status.as_u16() == 429 || status.is_server_error() {
                ApiError::retryable_after(message, super::parse_retry_after(&headers))
            } else {
                ApiError::fatal(message)
            };
            let rate_limits = crate::rate_limits::parse_all_rate_limits(&headers);
            if rate_limits.is_empty() {
                error
            } else {
                error.with_completed_response(None, rate_limits)
            }
        }
        error => ApiError::retryable(format!("failed to connect Responses WebSocket: {error}")),
    }
}
