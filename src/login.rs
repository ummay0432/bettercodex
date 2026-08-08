//! Fixed ChatGPT browser, device-code, status, and logout flows.
//!
//! This is a focused port of OpenAI Codex login behavior at commit
//! 1669c2403f793d0230065397dfc25f52b844244e. BetterCodex always uses the
//! ChatGPT issuer and the file credential store, so configurable providers,
//! keyrings, telemetry, model-provider metadata, and proxy routing are omitted.

use crate::auth;
use crate::http_client;
use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use reqwest::StatusCode as ReqwestStatusCode;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use serde::Serialize;
use serde::de;
use serde::de::Deserializer;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::io;
use std::io::Cursor;
use std::io::Read;
use std::io::Write;
use std::net::SocketAddr;
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tiny_http::Header;
use tiny_http::Request;
use tiny_http::Response;
use tiny_http::Server;
use tiny_http::StatusCode as TinyStatusCode;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_ISSUER: &str = "https://auth.openai.com";
const DEFAULT_PORT: u16 = 1455;
const FALLBACK_PORT: u16 = 1457;
const LIFE_SCIENCES_OAUTH_STATE_SUFFIX: &str = ".onboarding_entrypoint=life_sciences";
const REVOKE_TOKEN_URL: &str = "https://auth.openai.com/oauth/revoke";
const REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REVOKE_TOKEN_URL_OVERRIDE";
const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
const CLIENT_ID_OVERRIDE_ENV_VAR: &str = "CODEX_APP_SERVER_LOGIN_CLIENT_ID";
const REVOKE_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const ANSI_BLUE: &str = "\x1b[94m";
const ANSI_GRAY: &str = "\x1b[90m";
const ANSI_RESET: &str = "\x1b[0m";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginMode {
    Browser,
    DeviceCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginStatus {
    ChatGpt,
    AccessToken,
    NotLoggedIn,
}

pub(crate) async fn login(mode: LoginMode) -> Result<()> {
    clear_existing_auth().await;
    match mode {
        LoginMode::Browser => {
            let server = run_login_server().context("failed to start the login server")?;
            eprintln!(
                "Starting local login server on http://localhost:{}.\nIf your browser did not open, navigate to this URL to authenticate:\n\n{}\n\nOn a remote or headless machine? Use `bcodex login --device-auth` instead.",
                server.actual_port, server.auth_url
            );
            server
                .block_until_done()
                .await
                .context("ChatGPT login was not completed")
        }
        LoginMode::DeviceCode => run_device_code_login()
            .await
            .context("device code login was not completed"),
    }
}

pub(crate) fn status() -> Result<LoginStatus> {
    if std::env::var("CODEX_ACCESS_TOKEN").is_ok_and(|token| !token.trim().is_empty()) {
        return Ok(LoginStatus::AccessToken);
    }

    let path = auth::auth_file_path()?;
    match std::fs::metadata(&path) {
        Ok(_) => {
            auth::Auth::load()?;
            Ok(LoginStatus::ChatGpt)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(LoginStatus::NotLoggedIn),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect credentials at {}", path.display())),
    }
}

pub(crate) async fn logout() -> Result<bool> {
    let path = auth::auth_file_path()?;
    let document = load_auth_document(&path).ok().flatten();
    let _ = revoke_auth_tokens(document.as_ref()).await;
    remove_auth_file(&path).context("failed to remove stored authentication credentials")
}

async fn clear_existing_auth() {
    let _ = logout().await;
}

fn load_auth_document(path: &Path) -> io::Result<Option<Value>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn remove_auth_file(path: &Path) -> io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

struct LoginServer {
    auth_url: String,
    actual_port: u16,
    task: tokio::task::JoinHandle<io::Result<()>>,
}

impl LoginServer {
    async fn block_until_done(self) -> io::Result<()> {
        self.task
            .await
            .map_err(|error| io::Error::other(format!("login server thread panicked: {error:?}")))?
    }
}

fn run_login_server() -> io::Result<LoginServer> {
    let pkce = generate_pkce();
    let state = generate_state();
    let server = bind_server(DEFAULT_PORT)?;
    let actual_port = server
        .server_addr()
        .to_ip()
        .map(|address| address.port())
        .ok_or_else(|| io::Error::other("Unable to determine the server port"))?;
    let server = Arc::new(server);
    let redirect_uri = format!("http://localhost:{actual_port}/auth/callback");
    let auth_url = build_authorize_url(&redirect_uri, &pkce, &state);
    let _ = webbrowser::open(&auth_url);

    let (request_tx, mut request_rx) = tokio::sync::mpsc::channel::<Request>(16);
    let receiver_server = Arc::clone(&server);
    thread::spawn(move || {
        while let Ok(request) = receiver_server.recv() {
            if let Err(error) = request_tx.blocking_send(request) {
                eprintln!("Failed to send request to channel: {error}");
                break;
            }
        }
    });

    let task = tokio::spawn(async move {
        let result = loop {
            let Some(request) = request_rx.recv().await else {
                break Err(io::Error::other("Login was not completed"));
            };
            let url = request.url().to_string();
            match process_login_request(&url, &redirect_uri, &pkce, actual_port, &state).await {
                HandledRequest::Continue(response) => {
                    let _ = tokio::task::spawn_blocking(move || request.respond(response)).await;
                }
                HandledRequest::Exit {
                    status,
                    headers,
                    body,
                    result,
                } => {
                    let _ = tokio::task::spawn_blocking(move || {
                        send_response_with_disconnect(request, status, headers, body)
                    })
                    .await;
                    break result;
                }
            }
        };
        server.unblock();
        result
    });

    Ok(LoginServer {
        auth_url,
        actual_port,
        task,
    })
}

enum HandledRequest {
    Continue(Response<Cursor<Vec<u8>>>),
    Exit {
        status: TinyStatusCode,
        headers: Vec<Header>,
        body: Vec<u8>,
        result: io::Result<()>,
    },
}

async fn process_login_request(
    raw_url: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
    actual_port: u16,
    expected_state: &str,
) -> HandledRequest {
    let parsed_url = match url::Url::parse(&format!("http://localhost{raw_url}")) {
        Ok(url) => url,
        Err(error) => {
            eprintln!("URL parse error: {error}");
            return plain_response("Bad Request", 400);
        }
    };

    match parsed_url.path() {
        "/auth/callback" => {
            let parameters = parsed_url
                .query_pairs()
                .into_owned()
                .collect::<HashMap<_, _>>();
            let state_valid = parameters.get("state").is_some_and(|state| {
                state == expected_state
                    || state.strip_suffix(LIFE_SCIENCES_OAUTH_STATE_SUFFIX) == Some(expected_state)
            });
            if !state_valid {
                return plain_response("State mismatch", 400);
            }
            if let Some(error_code) = parameters.get("error") {
                let description = parameters.get("error_description").map(String::as_str);
                let message = oauth_callback_error_message(error_code, description);
                eprintln!("OAuth callback error: {message}");
                return login_error_response(
                    &message,
                    io::ErrorKind::PermissionDenied,
                    Some(error_code),
                    description,
                );
            }
            let Some(code) = parameters.get("code").filter(|code| !code.is_empty()) else {
                return login_error_response(
                    "Missing authorization code. Sign-in could not be completed.",
                    io::ErrorKind::InvalidData,
                    Some("missing_authorization_code"),
                    None,
                );
            };

            match exchange_code_for_tokens(redirect_uri, pkce, code).await {
                Ok(tokens) => {
                    let success_tokens = SuccessTokens {
                        id_token: tokens.id_token.clone(),
                        access_token: tokens.access_token.clone(),
                    };
                    let api_key = obtain_api_key(&tokens.id_token).await.ok();
                    if let Err(error) = persist_tokens(api_key, tokens).await {
                        eprintln!("Persist error: {error}");
                        return login_error_response(
                            "Sign-in completed but credentials could not be saved locally.",
                            io::ErrorKind::Other,
                            Some("persist_failed"),
                            Some(&error.to_string()),
                        );
                    }
                    let success_url = compose_success_url(actual_port, &success_tokens);
                    match Header::from_bytes("Location", success_url.as_bytes()) {
                        Ok(header) => HandledRequest::Continue(
                            Response::from_data(Vec::new())
                                .with_status_code(302)
                                .with_header(header),
                        ),
                        Err(_) => login_error_response(
                            "Sign-in completed but redirecting back to Codex failed.",
                            io::ErrorKind::Other,
                            Some("redirect_failed"),
                            None,
                        ),
                    }
                }
                Err(error) => {
                    eprintln!("Token exchange error: {error}");
                    login_error_response(
                        &format!("Token exchange failed: {error}"),
                        io::ErrorKind::Other,
                        Some("token_exchange_failed"),
                        None,
                    )
                }
            }
        }
        "/success" => HandledRequest::Exit {
            status: TinyStatusCode(200),
            headers: html_headers(),
            body: include_bytes!("login_assets/success_legacy.html").to_vec(),
            result: Ok(()),
        },
        "/cancel" => HandledRequest::Exit {
            status: TinyStatusCode(200),
            headers: Vec::new(),
            body: b"Login cancelled".to_vec(),
            result: Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Login cancelled",
            )),
        },
        _ => plain_response("Not Found", 404),
    }
}

struct SuccessTokens {
    id_token: String,
    access_token: String,
}

fn plain_response(body: &str, status: u16) -> HandledRequest {
    HandledRequest::Continue(Response::from_string(body).with_status_code(TinyStatusCode(status)))
}

fn html_headers() -> Vec<Header> {
    Header::from_bytes("Content-Type", "text/html; charset=utf-8")
        .ok()
        .into_iter()
        .collect()
}

fn send_response_with_disconnect(
    request: Request,
    status: TinyStatusCode,
    mut headers: Vec<Header>,
    body: Vec<u8>,
) -> io::Result<()> {
    let mut writer = request.into_writer();
    write!(
        writer,
        "HTTP/1.1 {} {}\r\n",
        status.0,
        status.default_reason_phrase()
    )?;
    headers.retain(|header| !header.field.equiv("Connection"));
    if let Ok(header) = Header::from_bytes("Connection", "close") {
        headers.push(header);
    }
    if let Ok(header) = Header::from_bytes("Content-Length", body.len().to_string()) {
        headers.push(header);
    }
    for header in headers {
        write!(
            writer,
            "{}: {}\r\n",
            header.field.as_str(),
            header.value.as_str()
        )?;
    }
    writer.write_all(b"\r\n")?;
    writer.write_all(&body)?;
    writer.flush()
}

fn build_authorize_url(redirect_uri: &str, pkce: &PkceCodes, state: &str) -> String {
    let query = [
        ("response_type", "code".to_string()),
        ("client_id", CLIENT_ID.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        (
            "scope",
            "openid profile email offline_access api.connectors.read api.connectors.invoke"
                .to_string(),
        ),
        ("code_challenge", pkce.code_challenge.clone()),
        ("code_challenge_method", "S256".to_string()),
        ("id_token_add_organizations", "true".to_string()),
        ("codex_cli_simplified_flow", "true".to_string()),
        ("state", state.to_string()),
        ("originator", originator()),
    ]
    .into_iter()
    .map(|(key, value)| format!("{key}={}", crate::url_encoding::encode(&value)))
    .collect::<Vec<_>>()
    .join("&");
    format!("{DEFAULT_ISSUER}/oauth/authorize?{query}")
}

fn originator() -> String {
    const DEFAULT: &str = "codex_cli_rs";
    std::env::var("CODEX_INTERNAL_ORIGINATOR_OVERRIDE")
        .ok()
        .filter(|value| HeaderValue::from_str(value).is_ok())
        .unwrap_or_else(|| DEFAULT.to_string())
}

fn generate_state() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn send_cancel_request(port: u16) -> io::Result<()> {
    let address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"GET /cancel HTTP/1.1\r\n")?;
    stream.write_all(format!("Host: 127.0.0.1:{port}\r\n").as_bytes())?;
    stream.write_all(b"Connection: close\r\n\r\n")?;
    let mut buffer = [0_u8; 64];
    let _ = stream.read(&mut buffer);
    Ok(())
}

fn bind_server(port: u16) -> io::Result<Server> {
    let preferred = format!("127.0.0.1:{port}");
    let fallback = format!("127.0.0.1:{FALLBACK_PORT}");
    let mut address = preferred;
    let mut cancel_attempted = false;
    let mut attempts = 0;
    let mut using_fallback = false;
    loop {
        match Server::http(&address) {
            Ok(server) => return Ok(server),
            Err(error) => {
                attempts += 1;
                let address_in_use = error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::AddrInUse);
                if !address_in_use {
                    return Err(io::Error::other(error));
                }
                if !cancel_attempted && !using_fallback {
                    cancel_attempted = true;
                    if let Err(error) = send_cancel_request(port) {
                        eprintln!("Failed to cancel previous login server: {error}");
                    }
                }
                thread::sleep(Duration::from_millis(200));
                if attempts < 10 {
                    continue;
                }
                if port == DEFAULT_PORT && !using_fallback {
                    address = fallback.clone();
                    attempts = 0;
                    using_fallback = true;
                    continue;
                }
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("Port {address} is already in use"),
                ));
            }
        }
    }
}

#[derive(Clone)]
struct PkceCodes {
    code_verifier: String,
    code_challenge: String,
}

fn generate_pkce() -> PkceCodes {
    let mut bytes = [0_u8; 64];
    rand::rng().fill_bytes(&mut bytes);
    let code_verifier = URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(digest);
    PkceCodes {
        code_verifier,
        code_challenge,
    }
}

#[derive(Clone)]
struct ExchangedTokens {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

async fn exchange_code_for_tokens(
    redirect_uri: &str,
    pkce: &PkceCodes,
    code: &str,
) -> io::Result<ExchangedTokens> {
    #[derive(Deserialize)]
    struct TokenResponse {
        id_token: String,
        access_token: String,
        refresh_token: String,
    }

    let client = raw_auth_client()?;
    let endpoint = format!("{DEFAULT_ISSUER}/oauth/token");
    let response = client
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
            crate::url_encoding::encode(code),
            crate::url_encoding::encode(redirect_uri),
            crate::url_encoding::encode(CLIENT_ID),
            crate::url_encoding::encode(&pkce.code_verifier)
        ))
        .send()
        .await
        .map_err(io::Error::other)?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.map_err(io::Error::other)?;
        return Err(io::Error::other(format!(
            "token endpoint returned status {status}: {}",
            token_endpoint_error(&body)
        )));
    }
    let tokens: TokenResponse = response.json().await.map_err(io::Error::other)?;
    Ok(ExchangedTokens {
        id_token: tokens.id_token,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
    })
}

async fn obtain_api_key(id_token: &str) -> io::Result<String> {
    #[derive(Deserialize)]
    struct ExchangeResponse {
        access_token: String,
    }

    let client = raw_auth_client()?;
    let endpoint = format!("{DEFAULT_ISSUER}/oauth/token");
    let response = client
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type={}&client_id={}&requested_token={}&subject_token={}&subject_token_type={}",
            crate::url_encoding::encode("urn:ietf:params:oauth:grant-type:token-exchange"),
            crate::url_encoding::encode(CLIENT_ID),
            crate::url_encoding::encode("openai-api-key"),
            crate::url_encoding::encode(id_token),
            crate::url_encoding::encode("urn:ietf:params:oauth:token-type:id_token")
        ))
        .send()
        .await
        .map_err(io::Error::other)?;
    if !response.status().is_success() {
        return Err(io::Error::other(format!(
            "api key exchange failed with status {}",
            response.status()
        )));
    }
    response
        .json::<ExchangeResponse>()
        .await
        .map(|response| response.access_token)
        .map_err(io::Error::other)
}

async fn persist_tokens(api_key: Option<String>, tokens: ExchangedTokens) -> io::Result<()> {
    tokio::task::spawn_blocking(move || {
        auth::save_login_tokens(
            api_key,
            tokens.id_token,
            tokens.access_token,
            tokens.refresh_token,
        )
        .map_err(io::Error::other)
    })
    .await
    .map_err(|error| io::Error::other(format!("persist task failed: {error}")))?
}

fn token_endpoint_error(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "unknown error".to_string();
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(description) = value
            .get("error_description")
            .and_then(Value::as_str)
            .filter(|description| !description.trim().is_empty())
        {
            return description.to_string();
        }
        if let Some(message) = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .filter(|message| !message.trim().is_empty())
        {
            return message.to_string();
        }
        if let Some(code) = value
            .get("error")
            .and_then(Value::as_str)
            .filter(|code| !code.trim().is_empty())
        {
            return code.to_string();
        }
    }
    trimmed.to_string()
}

fn compose_success_url(port: u16, tokens: &SuccessTokens) -> String {
    let id_claims = jwt_auth_claims(&tokens.id_token);
    let access_claims = jwt_auth_claims(&tokens.access_token);
    let completed_onboarding = id_claims
        .get("completed_platform_onboarding")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_org_owner = id_claims
        .get("is_org_owner")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let parameters = [
        ("id_token", tokens.id_token.as_str()),
        (
            "needs_setup",
            if !completed_onboarding && is_org_owner {
                "true"
            } else {
                "false"
            },
        ),
        (
            "org_id",
            id_claims
                .get("organization_id")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        (
            "project_id",
            id_claims
                .get("project_id")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        (
            "plan_type",
            access_claims
                .get("chatgpt_plan_type")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        ("platform_url", "https://platform.openai.com"),
    ]
    .into_iter()
    .map(|(key, value)| format!("{key}={}", crate::url_encoding::encode(value)))
    .collect::<Vec<_>>()
    .join("&");
    format!("http://localhost:{port}/success?{parameters}")
}

fn jwt_auth_claims(token: &str) -> serde_json::Map<String, Value> {
    let mut parts = token.split('.');
    let payload = match (parts.next(), parts.next(), parts.next()) {
        (Some(header), Some(payload), Some(signature))
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty() =>
        {
            payload
        }
        _ => return serde_json::Map::new(),
    };
    URL_SAFE_NO_PAD
        .decode(payload)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("https://api.openai.com/auth")
                .and_then(Value::as_object)
                .cloned()
        })
        .unwrap_or_default()
}

fn login_error_response(
    message: &str,
    kind: io::ErrorKind,
    error_code: Option<&str>,
    error_description: Option<&str>,
) -> HandledRequest {
    HandledRequest::Exit {
        status: TinyStatusCode(200),
        headers: html_headers(),
        body: render_login_error_page(message, error_code, error_description),
        result: Err(io::Error::new(kind, message.to_string())),
    }
}

fn is_missing_codex_entitlement(error_code: &str, description: Option<&str>) -> bool {
    error_code == "access_denied"
        && description.is_some_and(|description| {
            description
                .to_ascii_lowercase()
                .contains("missing_codex_entitlement")
        })
}

fn oauth_callback_error_message(error_code: &str, description: Option<&str>) -> String {
    if is_missing_codex_entitlement(error_code, description) {
        return "Codex is not enabled for your workspace. Contact your workspace administrator to request access to Codex.".to_string();
    }
    description
        .filter(|description| !description.trim().is_empty())
        .map_or_else(
            || format!("Sign-in failed: {error_code}"),
            |description| format!("Sign-in failed: {description}"),
        )
}

fn render_login_error_page(
    message: &str,
    error_code: Option<&str>,
    error_description: Option<&str>,
) -> Vec<u8> {
    let code = error_code.unwrap_or("unknown_error");
    let (title, display_message, description, help) = if is_missing_codex_entitlement(
        code,
        error_description,
    ) {
        (
            "You do not have access to Codex",
            "This account is not currently authorized to use Codex in this workspace.",
            "Contact your workspace administrator to request access to Codex.",
            "Contact your workspace administrator to get access to Codex, then return to Codex and try again.",
        )
    } else {
        (
            "Sign-in could not be completed",
            message,
            error_description.unwrap_or(message),
            "Return to Codex to retry, switch accounts, or contact your workspace admin if access is restricted.",
        )
    };
    include_str!("login_assets/error.html")
        .replace("{{error_title}}", &html_escape(title))
        .replace("{{error_message}}", &html_escape(display_message))
        .replace("{{error_code}}", &html_escape(code))
        .replace("{{error_description}}", &html_escape(description))
        .replace("{{error_help}}", &html_escape(help))
        .into_bytes()
}

fn html_escape(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
    output
}

#[derive(Debug)]
struct DeviceCode {
    verification_url: String,
    user_code: String,
    device_auth_id: String,
    interval: u64,
}

#[derive(Deserialize)]
struct UserCodeResponse {
    device_auth_id: String,
    #[serde(alias = "user_code", alias = "usercode")]
    user_code: String,
    #[serde(default, deserialize_with = "deserialize_interval")]
    interval: u64,
}

fn deserialize_interval<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer)?
        .trim()
        .parse()
        .map_err(de::Error::custom)
}

#[derive(Serialize)]
struct UserCodeRequest<'a> {
    client_id: &'a str,
}

#[derive(Serialize)]
struct TokenPollRequest<'a> {
    device_auth_id: &'a str,
    user_code: &'a str,
}

#[derive(Deserialize)]
struct DeviceCodeSuccess {
    authorization_code: String,
    code_challenge: String,
    code_verifier: String,
}

async fn run_device_code_login() -> io::Result<()> {
    let device_code = request_device_code().await?;
    print_device_code_prompt(&device_code.verification_url, &device_code.user_code);
    complete_device_code_login(device_code).await
}

async fn request_device_code() -> io::Result<DeviceCode> {
    let client = raw_auth_client()?;
    let endpoint = format!("{DEFAULT_ISSUER}/api/accounts/deviceauth/usercode");
    let response = client
        .post(endpoint)
        .json(&UserCodeRequest {
            client_id: CLIENT_ID,
        })
        .send()
        .await
        .map_err(io::Error::other)?;
    if !response.status().is_success() {
        if response.status() == ReqwestStatusCode::NOT_FOUND {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "device code login is not enabled for this Codex server. Use the browser login or verify the server URL.",
            ));
        }
        return Err(io::Error::other(format!(
            "device code request failed with status {}",
            response.status()
        )));
    }
    let response: UserCodeResponse = response.json().await.map_err(io::Error::other)?;
    Ok(DeviceCode {
        verification_url: format!("{DEFAULT_ISSUER}/codex/device"),
        user_code: response.user_code,
        device_auth_id: response.device_auth_id,
        interval: response.interval,
    })
}

async fn complete_device_code_login(device_code: DeviceCode) -> io::Result<()> {
    let response = poll_for_device_token(&device_code).await?;
    let tokens = exchange_code_for_tokens(
        &format!("{DEFAULT_ISSUER}/deviceauth/callback"),
        &PkceCodes {
            code_verifier: response.code_verifier,
            code_challenge: response.code_challenge,
        },
        &response.authorization_code,
    )
    .await?;
    persist_tokens(None, tokens).await
}

async fn poll_for_device_token(device_code: &DeviceCode) -> io::Result<DeviceCodeSuccess> {
    let client = raw_auth_client()?;
    let endpoint = format!("{DEFAULT_ISSUER}/api/accounts/deviceauth/token");
    let maximum_wait = Duration::from_secs(15 * 60);
    let started = Instant::now();
    loop {
        let response = client
            .post(&endpoint)
            .json(&TokenPollRequest {
                device_auth_id: &device_code.device_auth_id,
                user_code: &device_code.user_code,
            })
            .send()
            .await
            .map_err(io::Error::other)?;
        let status = response.status();
        if status.is_success() {
            return response.json().await.map_err(io::Error::other);
        }
        if matches!(
            status,
            ReqwestStatusCode::FORBIDDEN | ReqwestStatusCode::NOT_FOUND
        ) {
            if started.elapsed() >= maximum_wait {
                return Err(io::Error::other("device auth timed out after 15 minutes"));
            }
            let delay =
                Duration::from_secs(device_code.interval).min(maximum_wait - started.elapsed());
            tokio::time::sleep(delay).await;
            continue;
        }
        return Err(io::Error::other(format!(
            "device auth failed with status {status}"
        )));
    }
}

fn print_device_code_prompt(verification_url: &str, code: &str) {
    let prompt = format!(
        "\nWelcome to Codex [v{ANSI_GRAY}0.0.0{ANSI_RESET}]\n{ANSI_GRAY}OpenAI's command-line coding agent{ANSI_RESET}\n\
\nFollow these steps to sign in with ChatGPT using device code authorization:\n\
\n1. Open this link in your browser and sign in to your account\n   {ANSI_BLUE}{verification_url}{ANSI_RESET}\n\
\n2. Enter this one-time code {ANSI_GRAY}(expires in 15 minutes){ANSI_RESET}\n   {ANSI_BLUE}{code}{ANSI_RESET}\n\
\n{ANSI_GRAY}Continue only if you started this login in Codex. If a website or another person gave you this code, cancel.{ANSI_RESET}\n"
    );
    println!("{prompt}");
}

async fn revoke_auth_tokens(document: Option<&Value>) -> io::Result<()> {
    let Some((token, kind)) = revocable_token(document) else {
        return Ok(());
    };
    let endpoint = revoke_token_endpoint();
    let client = default_auth_client()?;
    let response = client
        .post(&endpoint)
        .timeout(REVOKE_HTTP_TIMEOUT)
        .json(&RevokeTokenRequest {
            token,
            token_type_hint: kind.as_str(),
            client_id: kind.client_id(),
        })
        .send()
        .await
        .map_err(io::Error::other)?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(io::Error::other(format!(
        "failed to revoke {}: {status}: {}",
        kind.as_str(),
        server_error_message(&body)
    )))
}

#[derive(Clone, Copy)]
enum RevokeTokenKind {
    Access,
    Refresh,
}

impl RevokeTokenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access_token",
            Self::Refresh => "refresh_token",
        }
    }

    fn client_id(self) -> Option<String> {
        match self {
            Self::Access => None,
            Self::Refresh => Some(oauth_client_id()),
        }
    }
}

#[derive(Serialize)]
struct RevokeTokenRequest<'a> {
    token: &'a str,
    token_type_hint: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
}

fn revocable_token(document: Option<&Value>) -> Option<(&str, RevokeTokenKind)> {
    let document = document?;
    let auth_mode = document.get("auth_mode").and_then(Value::as_str);
    if auth_mode.is_some_and(|mode| mode != "chatgpt")
        || (auth_mode.is_none()
            && document
                .get("OPENAI_API_KEY")
                .is_some_and(|key| !key.is_null()))
    {
        return None;
    }
    let tokens = document.get("tokens")?;
    let refresh = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty());
    refresh
        .map(|token| (token, RevokeTokenKind::Refresh))
        .or_else(|| {
            tokens
                .get("access_token")
                .and_then(Value::as_str)
                .filter(|token| !token.is_empty())
                .map(|token| (token, RevokeTokenKind::Access))
        })
}

fn revoke_token_endpoint() -> String {
    if let Ok(endpoint) = std::env::var(REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR) {
        return endpoint;
    }
    if let Ok(refresh_endpoint) = std::env::var(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR)
        && let Ok(mut endpoint) = url::Url::parse(&refresh_endpoint)
    {
        endpoint.set_path("/oauth/revoke");
        endpoint.set_query(None);
        return endpoint.to_string();
    }
    REVOKE_TOKEN_URL.to_string()
}

fn oauth_client_id() -> String {
    std::env::var(CLIENT_ID_OVERRIDE_ENV_VAR)
        .ok()
        .filter(|client_id| !client_id.trim().is_empty())
        .unwrap_or_else(|| CLIENT_ID.to_string())
}

fn server_error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            if body.is_empty() {
                "Unknown error".to_string()
            } else {
                body.to_string()
            }
        })
}

fn raw_auth_client() -> io::Result<reqwest::Client> {
    http_client::build_client(reqwest::Client::builder()).map_err(io::Error::other)
}

fn default_auth_client() -> io::Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    if let Ok(originator) = HeaderValue::from_str(&originator()) {
        headers.insert("originator", originator);
    }
    http_client::build_client(
        http_client::with_chatgpt_cloudflare_cookie_store(reqwest::Client::builder())
            .default_headers(headers)
            .user_agent(concat!("bettercodex/", env!("CARGO_PKG_VERSION"))),
    )
    .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::RevokeTokenKind;
    use super::build_authorize_url;
    use super::generate_pkce;
    use super::html_escape;
    use super::revocable_token;
    use serde_json::json;

    #[test]
    fn browser_authorize_url_retains_codex_oauth_contract() {
        let url = build_authorize_url(
            "http://localhost:1455/auth/callback",
            &generate_pkce(),
            "state",
        );
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("api.connectors.invoke"));
    }

    #[test]
    fn logout_prefers_refresh_token() {
        let document = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "access",
                "refresh_token": "refresh"
            }
        });
        assert!(matches!(
            revocable_token(Some(&document)),
            Some(("refresh", RevokeTokenKind::Refresh))
        ));
    }

    #[test]
    fn error_page_values_are_html_escaped() {
        assert_eq!(
            html_escape("<bad & \"worse\">"),
            "&lt;bad &amp; &quot;worse&quot;&gt;"
        );
    }
}
