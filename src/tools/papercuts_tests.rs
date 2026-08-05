use super::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Barrier;
use uuid::Uuid;

fn temporary_repository(label: &str) -> (PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("bettercodex-papercuts-{label}-{}", Uuid::new_v4()));
    let cwd = root.join("src/nested");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    (root, cwd)
}

#[test]
fn creates_the_log_at_the_git_root_and_normalizes_whitespace() {
    let (root, cwd) = temporary_repository("create");

    let output = log(
        &cwd,
        json!({"message": "  While running tests,\n  the documented path was stale.  "}),
        &CancellationToken::new(),
    )
    .unwrap();

    assert_eq!(output, json!({"path": "PAPERCUTS.md"}));
    assert_eq!(
        std::fs::read_to_string(root.join(FILE_NAME)).unwrap(),
        "# Papercuts\n\n- While running tests, the documented path was stale.\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn preserves_existing_content_and_supplies_a_missing_newline() {
    let (root, cwd) = temporary_repository("append");
    std::fs::write(root.join(FILE_NAME), "# Existing log\n\nKeep this.").unwrap();

    log(
        &cwd,
        json!({"message": "The formatter reported a misleading path."}),
        &CancellationToken::new(),
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(root.join(FILE_NAME)).unwrap(),
        "# Existing log\n\nKeep this.\n- The formatter reported a misleading path.\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_invalid_messages_without_creating_the_log() {
    let (root, cwd) = temporary_repository("invalid");
    let cases = [
        (" \n\t".to_string(), "requires a non-empty message"),
        (
            "x".repeat(MAX_MESSAGE_CHARS + 1),
            "message exceeds 1000 characters",
        ),
        (
            "contains a control \u{0007} character".to_string(),
            "contains unsupported control characters",
        ),
    ];

    for (message, expected) in cases {
        let error = log(&cwd, json!({"message": message}), &CancellationToken::new()).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }

    assert!(!root.join(FILE_NAME).exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn refuses_to_write_outside_a_git_worktree() {
    let cwd = std::env::temp_dir().join(format!("bettercodex-papercuts-none-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&cwd).unwrap();

    let error = log(
        &cwd,
        json!({"message": "The setup was unclear."}),
        &CancellationToken::new(),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("could not find a Git repository")
    );
    assert!(!cwd.join(FILE_NAME).exists());
    std::fs::remove_dir_all(cwd).unwrap();
}

#[test]
fn refuses_to_follow_a_papercuts_symlink() {
    let (root, cwd) = temporary_repository("symlink");
    let outside = root.with_extension("outside.md");
    std::fs::write(&outside, "unchanged\n").unwrap();
    std::os::unix::fs::symlink(&outside, root.join(FILE_NAME)).unwrap();

    let error = log(
        &cwd,
        json!({"message": "The setup was unclear."}),
        &CancellationToken::new(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("requires"));
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "unchanged\n");
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_file(outside).unwrap();
}

#[test]
fn concurrent_calls_create_one_header_and_two_complete_entries() {
    let (root, cwd) = temporary_repository("concurrent");
    let barrier = Arc::new(Barrier::new(2));
    let threads = ["First source of friction.", "Second source of friction."]
        .into_iter()
        .map(|message| {
            let cwd = cwd.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                log(&cwd, json!({"message": message}), &CancellationToken::new()).unwrap();
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }

    let contents = std::fs::read_to_string(root.join(FILE_NAME)).unwrap();
    let mut lines = contents.lines();
    let header = lines.next().unwrap_or_default();
    let separator = lines.next().unwrap_or_default();
    let mut entries = lines.collect::<Vec<_>>();
    entries.sort_unstable();
    assert_eq!(
        format!("{header}\n{separator}\n{}\n", entries.join("\n")),
        "# Papercuts\n\n- First source of friction.\n- Second source of friction.\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}
