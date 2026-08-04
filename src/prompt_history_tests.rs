use super::*;
use serde_json::json;

fn temporary_history_path() -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "bettercodex-prompt-history-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory.join(HISTORY_FILENAME)
}

#[test]
fn codex_history_is_loaded_appended_and_kept_private() {
    let path = temporary_history_path();
    let existing = [
        serde_json::to_string(&json!({
            "session_id": "codex-session",
            "ts": 1,
            "text": "older prompt",
        }))
        .unwrap(),
        "malformed row".to_string(),
        serde_json::to_string(&json!({
            "session_id": "codex-session",
            "ts": 2,
            "text": "newer prompt",
        }))
        .unwrap(),
    ]
    .join("\n");
    std::fs::write(&path, format!("{existing}\n")).unwrap();

    let mut history = PromptHistory::open_in(&path, "bettercodex-session").unwrap();
    assert_eq!(history.entries(), ["older prompt", "newer prompt"]);
    history.append("latest prompt").unwrap();
    drop(history);

    let history = PromptHistory::open_in(&path, "resumed-session").unwrap();
    assert_eq!(
        history.entries(),
        ["older prompt", "newer prompt", "latest prompt"]
    );
    let last = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .next_back()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .unwrap();
    let timestamp = last["ts"].clone();
    assert_eq!(
        last,
        json!({
            "session_id": "bettercodex-session",
            "ts": timestamp,
            "text": "latest prompt",
        })
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
