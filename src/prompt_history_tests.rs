use super::*;
use serde_json::json;

struct TemporaryHistory {
    directory: PathBuf,
    path: PathBuf,
}

impl TemporaryHistory {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "bettercodex-prompt-history-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(HISTORY_FILENAME);
        Self { directory, path }
    }
}

impl Drop for TemporaryHistory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn encoded_entry(timestamp: u64, text: &str) -> String {
    serde_json::to_string(&json!({
        "session_id": "codex-session",
        "ts": timestamp,
        "text": text,
    }))
    .unwrap()
}

fn read_all_newest_first(mut reader: PromptHistoryReader) -> Vec<String> {
    let mut entries = Vec::new();
    while reader.has_more() {
        entries.extend(reader.read_older().unwrap());
    }
    entries
}

#[test]
fn codex_history_is_loaded_appended_and_kept_private() {
    let history_file = TemporaryHistory::new();
    let existing = [
        encoded_entry(1, "older prompt"),
        "malformed row".to_string(),
        encoded_entry(2, "newer prompt"),
    ]
    .join("\n");
    std::fs::write(&history_file.path, format!("{existing}\n")).unwrap();

    let (mut history, reader) =
        PromptHistory::open_with_reader_in(&history_file.path, "bettercodex-session").unwrap();
    history.append("latest prompt").unwrap();
    assert_eq!(
        read_all_newest_first(reader),
        ["newer prompt", "older prompt"],
        "a reader remains pinned to the history snapshot from when it was opened"
    );
    drop(history);

    let (_history, reader) =
        PromptHistory::open_with_reader_in(&history_file.path, "resumed-session").unwrap();
    assert_eq!(
        read_all_newest_first(reader),
        ["latest prompt", "newer prompt", "older prompt"]
    );
    let last = std::fs::read_to_string(&history_file.path)
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
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&history_file.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn persistent_history_is_loaded_in_bounded_batches() {
    let history_file = TemporaryHistory::new();
    let rows = (0..300)
        .map(|index| encoded_entry(index, &format!("prompt {index}")))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&history_file.path, format!("{rows}\n")).unwrap();

    let (_history, mut reader) =
        PromptHistory::open_with_reader_in(&history_file.path, "batch-session").unwrap();
    let first = reader.read_older().unwrap();
    assert_eq!(first.len(), MAX_BATCH_ROWS);
    assert_eq!(first.first().map(String::as_str), Some("prompt 299"));
    assert_eq!(first.last().map(String::as_str), Some("prompt 172"));
    assert!(reader.has_more());

    let mut loaded = first;
    while reader.has_more() {
        loaded.extend(reader.read_older().unwrap());
    }
    assert_eq!(loaded.len(), 300);
    assert_eq!(loaded.last().map(String::as_str), Some("prompt 0"));
}

#[test]
fn oversized_history_rows_make_forward_progress() {
    let history_file = TemporaryHistory::new();
    let oversized = "x".repeat(MAX_BATCH_BYTES + 1);
    let rows = [
        encoded_entry(1, "oldest"),
        encoded_entry(2, &oversized),
        encoded_entry(3, "newest"),
    ]
    .join("\n");
    std::fs::write(&history_file.path, format!("{rows}\n")).unwrap();

    let (_history, mut reader) =
        PromptHistory::open_with_reader_in(&history_file.path, "oversized-session").unwrap();
    assert_eq!(reader.read_older().unwrap(), ["newest"]);
    assert_eq!(reader.read_older().unwrap(), [oversized]);
    assert_eq!(reader.read_older().unwrap(), ["oldest"]);
    assert!(!reader.has_more());
}

#[test]
fn appends_follow_history_rotation() {
    let history_file = TemporaryHistory::new();
    let mut history = PromptHistory::open_in(&history_file.path, "rotation-session").unwrap();
    history.append("before rotation").unwrap();

    let archived = history_file.directory.join("history.previous.jsonl");
    std::fs::rename(&history_file.path, &archived).unwrap();
    std::fs::write(
        &history_file.path,
        format!("{}\n", encoded_entry(1, "replacement entry")),
    )
    .unwrap();

    history.append("after rotation").unwrap();
    let (_current, reader) =
        PromptHistory::open_with_reader_in(&history_file.path, "current-session").unwrap();
    assert_eq!(
        read_all_newest_first(reader),
        ["after rotation", "replacement entry"]
    );

    let archived_text = std::fs::read_to_string(archived).unwrap();
    let archived_entry: serde_json::Value = serde_json::from_str(archived_text.trim()).unwrap();
    assert_eq!(archived_entry["text"], "before rotation");
}

#[test]
fn appends_reject_a_symbolic_link_substituted_after_rotation() {
    use std::os::unix::fs::symlink;

    let history_file = TemporaryHistory::new();
    let mut history = PromptHistory::open_in(&history_file.path, "rotation-session").unwrap();
    history.append("before rotation").unwrap();

    let archived = history_file.directory.join("history.previous.jsonl");
    std::fs::rename(&history_file.path, &archived).unwrap();
    let target = history_file.directory.join("unrelated.jsonl");
    std::fs::write(&target, "must remain unchanged\n").unwrap();
    symlink(&target, &history_file.path).unwrap();

    assert!(history.append("must not be redirected").is_err());
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        "must remain unchanged\n"
    );
}
