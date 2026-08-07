use super::*;
use std::future::pending;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

fn unsigned_jwt(payload: Value) -> String {
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("serialize claims"));
    format!("header.{encoded}.signature")
}

#[test]
fn extracts_expiration_and_account_from_chatgpt_claims() {
    let token = unsigned_jwt(serde_json::json!({
        "exp": 1_900_000_000_u64,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "account-123"
        }
    }));

    assert_eq!(expiration_from_jwt(&token), Some(1_900_000_000));
    assert_eq!(account_id_from_jwt(&token).as_deref(), Some("account-123"));
}

#[test]
fn renders_unix_epoch_as_rfc3339() {
    assert_eq!(civil_date_from_unix_days(0), (1970, 1, 1));
    assert_eq!(civil_date_from_unix_days(20_000), (2024, 10, 4));
}

#[tokio::test]
async fn refresh_uses_newer_same_account_credentials_from_disk() {
    let directory = temporary_directory("reload");
    let path = directory.join("auth.json");
    std::fs::write(
        &path,
        stored_auth("old-access", "old-refresh", "account-123"),
    )
    .unwrap();
    let mut auth = Auth::load_from_file(path.clone()).unwrap();
    auth.refresh_url = Cow::Borrowed("http://127.0.0.1:0/unreachable");

    std::fs::write(
        &path,
        stored_auth("new-access", "new-refresh", "account-123"),
    )
    .unwrap();
    auth.force_refresh(&reqwest::Client::new()).await.unwrap();

    assert_eq!(auth.access_token, "new-access");
    assert_eq!(auth.refresh_token.as_deref(), Some("new-refresh"));
    assert_eq!(auth.refresh_url, "http://127.0.0.1:0/unreachable");
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn refresh_rejects_credentials_for_a_different_account() {
    let directory = temporary_directory("account-mismatch");
    let path = directory.join("auth.json");
    std::fs::write(
        &path,
        stored_auth("old-access", "old-refresh", "account-123"),
    )
    .unwrap();
    let mut auth = Auth::load_from_file(path.clone()).unwrap();
    auth.refresh_url = Cow::Borrowed("http://127.0.0.1:0/unreachable");

    std::fs::write(
        &path,
        stored_auth("new-access", "new-refresh", "account-456"),
    )
    .unwrap();
    let error = auth
        .force_refresh(&reqwest::Client::new())
        .await
        .unwrap_err();

    assert!(error.to_string().contains("changed accounts"), "{error:#}");
    assert_eq!(auth.access_token, "old-access");
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn refresh_error_body_does_not_wait_for_an_unbounded_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1_024];
        let _ = stream.read(&mut request).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        stream
            .write_all(&vec![b'x'; MAX_REFRESH_ERROR_BODY_BYTES + 1_024])
            .await
            .unwrap();
        stream.flush().await.unwrap();
        pending::<()>().await;
    });
    let response = reqwest::Client::new()
        .get(format!("http://{address}"))
        .send()
        .await
        .unwrap();

    let body = tokio::time::timeout(Duration::from_secs(2), bounded_error_body(response))
        .await
        .expect("bounded response reader waited for the unfinished body");
    server.abort();

    assert_eq!(body.len(), MAX_REFRESH_ERROR_BODY_CHARS);
    assert!(body.bytes().all(|byte| byte == b'x'));
}

fn stored_auth(access_token: &str, refresh_token: &str, account_id: &str) -> String {
    serde_json::json!({
        "tokens": {
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": account_id,
        }
    })
    .to_string()
}

fn temporary_directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "bettercodex-auth-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4(),
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
