use super::*;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

fn file_match(path: &str, indices: &[u32]) -> FileMatch {
    FileMatch {
        score: 100,
        path: PathBuf::from(path),
        match_type: MatchType::File,
        root: PathBuf::from("/repo"),
        indices: Some(indices.to_vec()),
    }
}

#[test]
fn active_token_follows_the_cursor_without_crossing_whitespace() {
    assert_eq!(
        active_token("inspect @src/tui/view.rs", "inspect @src/tui/view.rs".len()),
        Some(ActiveToken {
            range: "inspect ".len().."inspect @src/tui/view.rs".len(),
            query: "src/tui/view.rs".to_string(),
        })
    );
    assert_eq!(
        active_token("@", 1),
        Some(ActiveToken {
            range: 0..1,
            query: String::new(),
        })
    );
    assert_eq!(active_token("inspect @src ", "inspect @src ".len()), None);
    assert_eq!(
        active_token("mail@example.com", "mail@example.com".len()),
        None
    );
    assert_eq!(active_token("@src\nnext", "@src\n".len()), None);
}

#[test]
fn popup_rejects_stale_results_and_wraps_selection() {
    let mut popup = FileSearchPopup::default();
    popup.sync("open @vie", "open @vie".len());
    popup.apply_update(FileSearchUpdate::Matches {
        query: "old".to_string(),
        matches: vec![file_match("old.rs", &[0])],
    });
    assert_eq!(popup.selected_path(), None);

    popup.apply_update(FileSearchUpdate::Matches {
        query: "vie".to_string(),
        matches: vec![
            file_match("src/tui/view.rs", &[8, 9, 10]),
            file_match("src/view_model.rs", &[4, 5, 6]),
        ],
    });
    assert_eq!(
        popup.selected_path(),
        Some((
            "open ".len().."open @vie".len(),
            "src/tui/view.rs".to_string()
        ))
    );
    popup.move_up();
    assert_eq!(
        popup.selected_path().map(|(_, path)| path),
        Some("src/view_model.rs".to_string())
    );
    popup.move_down();
    assert_eq!(
        popup.selected_path().map(|(_, path)| path),
        Some("src/tui/view.rs".to_string())
    );
}

#[test]
fn rendered_rows_show_selection_type_and_fuzzy_emphasis() {
    let line = file_match_line(&file_match("src/tui/view.rs", &[8, 9, 10]), true, 50);
    let rendered = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(rendered.starts_with("> view.rs  src/tui/"), "{rendered}");
    assert!(rendered.ends_with("File"), "{rendered}");
    assert!(
        line.spans
            .iter()
            .all(|span| span.style.fg == Some(Color::Cyan)
                && span.style.add_modifier.contains(Modifier::BOLD))
    );

    let unselected = file_match_line(&file_match("src/tui/view.rs", &[8]), false, 50);
    assert!(unselected.spans.iter().any(|span| {
        span.content.contains('v') && span.style.add_modifier.contains(Modifier::BOLD)
    }));
}

#[tokio::test]
async fn manager_returns_codex_fuzzy_matches_from_the_repository_tree() {
    let root = std::env::temp_dir().join(format!("bcodex-file-search-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src/tui")).unwrap();
    std::fs::write(root.join("src/tui/view.rs"), "fn render() {}\n").unwrap();
    std::fs::write(root.join("README.md"), "fixture\n").unwrap();

    let (updates_tx, mut updates_rx) = unbounded_channel();
    let manager = FileSearchManager::new(root.clone(), updates_tx);
    manager.on_query_changed("tvi");

    let matching_update = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = updates_rx.recv().await.expect("search manager stays alive");
            if let FileSearchUpdate::Matches { query, matches, .. } = &update
                && query == "tvi"
                && matches
                    .iter()
                    .any(|file_match| file_match.path == Path::new("src/tui/view.rs"))
            {
                break update;
            }
        }
    })
    .await
    .expect("fuzzy search completed");
    assert!(matches!(matching_update, FileSearchUpdate::Matches { .. }));

    manager.on_query_changed("");
    drop(manager);
    std::fs::remove_dir_all(root).unwrap();
}
