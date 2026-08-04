use super::ApiError;
use super::ApiResult;
use futures::SinkExt;
use futures::StreamExt;
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async_with_config;
use tungstenite::Message;
use tungstenite::client::IntoClientRequest;
use tungstenite::extensions::ExtensionsConfig;
use tungstenite::extensions::compression::deflate::DeflateConfig;
use tungstenite::protocol::WebSocketConfig;

const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) struct WebSocketConnection {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl WebSocketConnection {
    pub(super) async fn connect(url: &str, headers: &HeaderMap) -> ApiResult<(Self, HeaderMap)> {
        let mut request = url.into_client_request().map_err(|error| {
            ApiError::fatal(format!("failed to build WebSocket request: {error}"))
        })?;
        request.headers_mut().extend(headers.clone());
        let (stream, response) = timeout(
            WEBSOCKET_CONNECT_TIMEOUT,
            connect_async_with_config(request, Some(websocket_config()), false),
        )
        .await
        .map_err(|_| ApiError::retryable("timed out connecting Responses WebSocket"))?
        .map_err(map_connect_error)?;
        Ok((Self { stream }, response.headers().clone()))
    }

    pub(super) async fn send(&mut self, request: &Value, idle_timeout: Duration) -> ApiResult<()> {
        let encoded = serde_json::to_string(request).map_err(|error| {
            ApiError::fatal(format!("failed to encode WebSocket request: {error}"))
        })?;
        timeout(
            idle_timeout,
            self.stream.send(Message::Text(encoded.into())),
        )
        .await
        .map_err(|_| ApiError::retryable("timed out sending Responses WebSocket request"))?
        .map_err(|error| ApiError::retryable(format!("failed to send WebSocket request: {error}")))
    }

    pub(super) async fn next_text(&mut self, idle_timeout: Duration) -> ApiResult<Option<String>> {
        loop {
            let message = timeout(idle_timeout, self.stream.next())
                .await
                .map_err(|_| ApiError::retryable("timed out waiting for Responses WebSocket"))?;
            let Some(message) = message else {
                return Err(ApiError::retryable(
                    "WebSocket closed before response.completed",
                ));
            };
            match message {
                Ok(Message::Text(text)) => return Ok(Some(text.to_string())),
                Ok(Message::Ping(payload)) => {
                    self.stream
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|error| {
                            ApiError::retryable(format!(
                                "failed to answer Responses WebSocket ping: {error}"
                            ))
                        })?;
                }
                Ok(Message::Pong(_) | Message::Frame(_)) => {}
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
                Err(error) => {
                    return Err(ApiError::retryable(format!(
                        "Responses WebSocket failed: {error}"
                    )));
                }
            }
        }
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
            let body = response
                .body()
                .as_ref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_default();
            if status.as_u16() == 401 {
                ApiError::unauthorized(format!("Responses WebSocket authentication failed: {body}"))
            } else if matches!(status.as_u16(), 404 | 405 | 426) {
                ApiError::websocket_unavailable(format!(
                    "Responses WebSocket upgrade failed with {status}: {body}"
                ))
            } else if status.as_u16() == 429 || status.is_server_error() {
                ApiError::retryable(format!(
                    "Responses WebSocket upgrade failed with {status}: {body}"
                ))
            } else {
                ApiError::fatal(format!(
                    "Responses WebSocket upgrade failed with {status}: {body}"
                ))
            }
        }
        error => ApiError::retryable(format!("failed to connect Responses WebSocket: {error}")),
    }
}
