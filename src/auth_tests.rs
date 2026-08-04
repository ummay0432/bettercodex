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
