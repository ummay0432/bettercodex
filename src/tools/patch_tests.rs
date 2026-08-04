use super::apply;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("bettercodex-patch-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn applies_add_update_delete_and_move() {
    let root = TempDir::new();
    std::fs::write(root.path().join("update.txt"), "one\ntwo\n").unwrap();
    std::fs::write(root.path().join("delete.txt"), "old\n").unwrap();
    std::fs::write(root.path().join("move.txt"), "move me\n").unwrap();

    let result = apply(
        root.path(),
        "*** Begin Patch\n*** Add File: nested/new.txt\n+new\n*** Update File: update.txt\n@@\n one\n-two\n+three\n*** Delete File: delete.txt\n*** Update File: move.txt\n*** Move to: moved.txt\n@@\n move me\n*** End Patch\n",
    )
    .unwrap();

    assert_eq!(
        result,
        concat!(
            "Success. Updated the following files:\n",
            "A nested/new.txt\n",
            "M update.txt\n",
            "M moved.txt\n",
            "D delete.txt\n",
        )
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("nested/new.txt")).unwrap(),
        "new\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("update.txt")).unwrap(),
        "one\nthree\n"
    );
    assert!(!root.path().join("delete.txt").exists());
    assert!(!root.path().join("move.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("moved.txt")).unwrap(),
        "move me\n"
    );
}

#[test]
fn uses_anchors_and_end_of_file() {
    let root = TempDir::new();
    std::fs::write(root.path().join("file.txt"), "first\nsection\nold\nlast\n").unwrap();

    apply(
        root.path(),
        "*** Begin Patch\n*** Update File: file.txt\n@@ section\n-old\n+new\n@@\n last\n+tail\n*** End of File\n*** End Patch",
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(root.path().join("file.txt")).unwrap(),
        "first\nsection\nnew\nlast\ntail\n"
    );
}

#[test]
fn patches_paths_outside_the_working_directory_with_user_permissions() {
    let root = TempDir::new();
    let work = root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    apply(
        &work,
        "*** Begin Patch\n*** Add File: ../escape.txt\n+no\n*** End Patch\n",
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(root.path().join("escape.txt")).unwrap(),
        "no\n",
    );
}

#[test]
fn preserves_codex_partial_application_on_later_failure() {
    let root = TempDir::new();
    let error = apply(
        root.path(),
        "*** Begin Patch\n*** Add File: staged.txt\n+not yet\n*** Update File: missing.txt\n@@\n-old\n+new\n*** End Patch\n",
    )
    .unwrap_err();

    assert!(error.to_string().contains("Failed to read file to update"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("staged.txt")).unwrap(),
        "not yet\n",
    );
}

#[test]
fn leaves_files_unchanged_when_context_does_not_match() {
    let root = TempDir::new();
    let path = root.path().join("file.txt");
    std::fs::write(&path, "actual\n").unwrap();

    let error = apply(
        root.path(),
        "*** Begin Patch\n*** Update File: file.txt\n@@\n-expected\n+replacement\n*** End Patch\n",
    )
    .unwrap_err();

    assert!(error.to_string().contains("Failed to find expected lines"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "actual\n");
}

#[test]
fn matches_codex_whitespace_and_unicode_tolerance() {
    let root = TempDir::new();
    let path = root.path().join("file.txt");
    std::fs::write(&path, "  title — ‘quoted’  \n").unwrap();

    apply(
        root.path(),
        "*** Begin Patch\n*** Update File: file.txt\n@@\n-title - 'quoted'\n+replaced\n*** End Patch",
    )
    .unwrap();

    assert_eq!(std::fs::read_to_string(path).unwrap(), "replaced\n");
}

#[test]
fn accepts_codex_lenient_heredoc_wrapper_and_marker_padding() {
    let root = TempDir::new();
    apply(
        root.path(),
        "<<'EOF'\n  *** Begin Patch  \n*** Add File: made.txt\n+yes\n  *** End Patch  \nEOF\n",
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(root.path().join("made.txt")).unwrap(),
        "yes\n",
    );
}

#[test]
fn matches_codex_empty_add_and_empty_patch_failures() {
    let root = TempDir::new();
    apply(
        root.path(),
        "*** Begin Patch\n*** Add File: empty.txt\n*** End Patch",
    )
    .unwrap();
    assert_eq!(
        std::fs::read(root.path().join("empty.txt")).unwrap(),
        Vec::<u8>::new(),
    );

    let error = apply(root.path(), "*** Begin Patch\n*** End Patch").unwrap_err();
    assert_eq!(error.to_string(), "No files were modified.");
}

#[test]
fn pure_additions_preserve_codex_trailing_blank_line_position() {
    let root = TempDir::new();
    let path = root.path().join("file.txt");
    std::fs::write(&path, "first\n\n").unwrap();

    apply(
        root.path(),
        "*** Begin Patch\n*** Update File: file.txt\n@@\n+inserted\n*** End Patch",
    )
    .unwrap();

    assert_eq!(std::fs::read_to_string(path).unwrap(), "first\ninserted\n");
}

#[test]
fn permits_codex_blank_lines_after_end_of_file_marker() {
    let root = TempDir::new();
    let path = root.path().join("file.txt");
    std::fs::write(&path, "old\n").unwrap();

    apply(
        root.path(),
        "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n+new\n*** End of File\n\n*** End Patch",
    )
    .unwrap();

    assert_eq!(std::fs::read_to_string(path).unwrap(), "new\n");
}
