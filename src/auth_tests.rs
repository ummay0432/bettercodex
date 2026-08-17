use super::*;

fn unsigned_jwt(payload: Value) -> String {
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("serialize claims"));
    format!("header.{encoded}.signature")
}

#[test]
fn extracts_expiration_and_account_from_chatgpt_claims() {
    let token = unsigned_jwt(serde_json::json!({
        "exp": 1_900_000_000_u64,
        "email": "user@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "account-123",
            "chatgpt_plan_type": "pro"
        }
    }));

    assert_eq!(expiration_from_jwt(&token), Some(1_900_000_000));
    assert_eq!(account_id_from_jwt(&token).as_deref(), Some("account-123"));
    assert_eq!(
        chatgpt_account_from_tokens(None, &token),
        ChatGptAccount {
            email: Some("user@example.com".to_string()),
            plan: Some("Pro".to_string()),
        }
    );
}

#[test]
fn renders_unix_epoch_as_rfc3339() {
    assert_eq!(civil_date_from_unix_days(0), (1970, 1, 1));
    assert_eq!(civil_date_from_unix_days(20_000), (2024, 10, 4));
}

#[test]
fn private_auth_replacement_is_atomic_across_concurrent_writers() {
    let directory = std::env::temp_dir().join(format!(
        "bettercodex-auth-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("auth.json");
    std::fs::write(&path, b"stale").unwrap();
    let colliding_process_temp = directory.join(format!(".auth.json.tmp-{}", std::process::id()));
    std::fs::write(&colliding_process_temp, b"unrelated").unwrap();
    let documents = (0..8)
        .map(|writer| serde_json::json!({"writer": writer}))
        .collect::<Vec<_>>();
    let barrier = std::sync::Barrier::new(documents.len());

    std::thread::scope(|scope| {
        let handles = documents
            .iter()
            .map(|document| {
                let path = &path;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    write_private_json(path, document)
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
    });

    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(documents.contains(&stored));
    assert_eq!(
        std::fs::read(&colliding_process_temp).unwrap(),
        b"unrelated"
    );
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn oversized_auth_file_is_rejected_before_loading() {
    let directory = std::env::temp_dir().join(format!(
        "bettercodex-auth-limit-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("auth.json");
    let file = File::create(&path).unwrap();
    file.set_len(u64::try_from(MAX_AUTH_FILE_BYTES).unwrap() + 1)
        .unwrap();

    let error = read_auth_document(&path).unwrap_err();

    assert!(error.to_string().contains("exceed the 1 MiB limit"));
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn unrefreshable_access_token_remains_usable_until_rejected() {
    crate::http_client::ensure_rustls_crypto_provider();
    let client = reqwest::Client::new();
    let auth = SharedAuth::new(Auth {
        access_token: "externally-managed-token".to_string(),
        refresh_token: None,
        account_id: Some("account-test".to_string()),
        account: ChatGptAccount::default(),
        expires_at: Some(unix_timestamp().unwrap() + 60),
        last_refresh: None,
        refresh_url: Cow::Borrowed(REFRESH_URL),
        storage: None,
    });

    let snapshot = auth.refreshed_snapshot(&client).await.unwrap();

    assert_eq!(
        snapshot.authorization.to_str().unwrap(),
        "Bearer externally-managed-token"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_refresh_failure_uses_the_current_token() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let refresh_url = format!("http://{}", server.server_addr());
    let server_task = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .expect("refresh request timed out");
        request
            .respond(
                tiny_http::Response::from_string("temporary refresh outage").with_status_code(500),
            )
            .unwrap();
    });
    crate::http_client::ensure_rustls_crypto_provider();
    let client = reqwest::Client::new();
    let auth = SharedAuth::new(Auth {
        access_token: "still-valid-token".to_string(),
        refresh_token: Some("initial-refresh-token".to_string()),
        account_id: Some("account-test".to_string()),
        account: ChatGptAccount::default(),
        expires_at: Some(unix_timestamp().unwrap() + 60),
        last_refresh: Some(unix_timestamp().unwrap()),
        refresh_url: Cow::Owned(refresh_url),
        storage: None,
    });

    let snapshot = auth.refreshed_snapshot(&client).await.unwrap();
    server_task.join().unwrap();

    assert_eq!(
        snapshot.authorization.to_str().unwrap(),
        "Bearer still-valid-token"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_refresh_timestamp_refreshes_a_token_without_expiration() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let refresh_url = format!("http://{}", server.server_addr());
    let server_task = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .expect("refresh request timed out");
        let content_type = tiny_http::Header::from_bytes(
            b"content-type".as_slice(),
            b"application/json".as_slice(),
        )
        .unwrap();
        request
            .respond(
                tiny_http::Response::from_string(
                    serde_json::json!({
                        "access_token": "fresh-token",
                        "refresh_token": "fresh-refresh-token",
                    })
                    .to_string(),
                )
                .with_header(content_type),
            )
            .unwrap();
    });
    crate::http_client::ensure_rustls_crypto_provider();
    let client = reqwest::Client::new();
    let auth = SharedAuth::new(Auth {
        access_token: "token-without-expiration".to_string(),
        refresh_token: Some("initial-refresh-token".to_string()),
        account_id: Some("account-test".to_string()),
        account: ChatGptAccount::default(),
        expires_at: None,
        last_refresh: Some(unix_timestamp().unwrap() - REFRESH_INTERVAL.as_secs() - 1),
        refresh_url: Cow::Owned(refresh_url),
        storage: None,
    });

    let snapshot = auth.refreshed_snapshot(&client).await.unwrap();
    server_task.join().unwrap();

    assert_eq!(
        snapshot.authorization.to_str().unwrap(),
        "Bearer fresh-token"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_unauthorized_recovery_reuses_the_first_refresh() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let refresh_url = format!("http://{}", server.server_addr());
    let server_task = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .expect("refresh request timed out");
        let content_type = tiny_http::Header::from_bytes(
            b"content-type".as_slice(),
            b"application/json".as_slice(),
        )
        .unwrap();
        request
            .respond(
                tiny_http::Response::from_string(
                    serde_json::json!({
                        "access_token": "fresh-token",
                        "refresh_token": "fresh-refresh-token",
                    })
                    .to_string(),
                )
                .with_header(content_type),
            )
            .unwrap();
    });
    crate::http_client::ensure_rustls_crypto_provider();
    let client = reqwest::Client::new();
    let auth = SharedAuth::new(Auth {
        access_token: "rejected-token".to_string(),
        refresh_token: Some("initial-refresh-token".to_string()),
        account_id: Some("account-test".to_string()),
        account: ChatGptAccount::default(),
        expires_at: None,
        last_refresh: None,
        refresh_url: Cow::Owned(refresh_url),
        storage: None,
    });
    let rejected = auth.refreshed_snapshot(&client).await.unwrap();

    let (first, second) = tokio::join!(
        auth.refreshed_snapshot_after_unauthorized(&client, &rejected),
        auth.refreshed_snapshot_after_unauthorized(&client, &rejected),
    );
    server_task.join().unwrap();

    for snapshot in [first.unwrap(), second.unwrap()] {
        assert_eq!(
            snapshot.authorization.to_str().unwrap(),
            "Bearer fresh-token"
        );
        assert_eq!(
            snapshot
                .account_id
                .as_ref()
                .and_then(|value| value.to_str().ok()),
            Some("account-test")
        );
    }
}
