use super::*;

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
