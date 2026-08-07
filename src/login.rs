//! ChatGPT login commands ported through Codex's login crate.

use crate::auth;
use anyhow::Context;
use anyhow::Result;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthRouteConfig;
use codex_login::CLIENT_ID;
use codex_login::ServerOptions;
use codex_login::logout_with_revoke;
use codex_login::run_device_code_login;
use codex_login::run_login_server;

const CREDENTIALS_STORE: AuthCredentialsStoreMode = AuthCredentialsStoreMode::File;

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
    let codex_home = auth::codex_home()?;
    let auth_route_config = auth_route_config();
    clear_existing_auth(&codex_home, &auth_route_config).await;
    let options = ServerOptions::new(
        codex_home,
        CLIENT_ID.to_string(),
        None,
        CREDENTIALS_STORE,
        AuthKeyringBackendKind::default(),
        auth_route_config,
    );

    match mode {
        LoginMode::Browser => {
            let server = run_login_server(options).context("failed to start the login server")?;
            eprintln!(
                "Starting local login server on http://localhost:{}.\nIf your browser did not open, navigate to this URL to authenticate:\n\n{}\n\nOn a remote or headless machine? Use `bcodex login --device-auth` instead.",
                server.actual_port, server.auth_url
            );
            server
                .block_until_done()
                .await
                .context("ChatGPT login was not completed")
        }
        LoginMode::DeviceCode => run_device_code_login(options)
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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(LoginStatus::NotLoggedIn),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect credentials at {}", path.display())),
    }
}

pub(crate) async fn logout() -> Result<bool> {
    logout_with_revoke(
        &auth::codex_home()?,
        CREDENTIALS_STORE,
        AuthKeyringBackendKind::default(),
        &auth_route_config(),
    )
    .await
    .context("failed to remove stored authentication credentials")
}

async fn clear_existing_auth(codex_home: &std::path::Path, auth_route_config: &AuthRouteConfig) {
    let _ = logout_with_revoke(
        codex_home,
        CREDENTIALS_STORE,
        AuthKeyringBackendKind::default(),
        auth_route_config,
    )
    .await;
}

fn auth_route_config() -> AuthRouteConfig {
    AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
        OutboundProxyPolicy::ReqwestDefault,
    ))
}
