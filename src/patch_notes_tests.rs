use super::*;
use std::path::PathBuf;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("bettercodex-patch-notes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn state_path(&self) -> PathBuf {
        self.0.join(STATE_FILE_NAME)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn version(value: &str) -> Version {
    Version::parse(value).unwrap()
}

const RELEASES: &str = "# Changelog\n\n\
## [Unreleased]\n\n- Not shipped\n\n\
## [0.1.5] - 2026-08-20\n\n- Newer\n\n\
## [0.1.4] - 2026-08-10\n\n- First tracked\n";

#[test]
fn released_notes_are_bounded_by_the_binary_and_ordered_for_scrollback() -> Result<()> {
    assert_eq!(
        notes_between(RELEASES, None, version("0.1.4"))?,
        Some("## [0.1.4] - 2026-08-10\n\n- First tracked".to_string())
    );
    assert_eq!(
        notes_between(RELEASES, Some(version("0.1.3")), version("0.1.5"))?,
        Some(
            "## [0.1.4] - 2026-08-10\n\n- First tracked\n\n\
             ## [0.1.5] - 2026-08-20\n\n- Newer"
                .to_string()
        )
    );
    Ok(())
}

#[test]
fn existing_users_see_each_new_release_once() -> Result<()> {
    let root = TempDir::new();
    let path = root.state_path();

    assert_eq!(
        for_startup_at(&path, RELEASES, version("0.1.4"), true)?,
        Some("## [0.1.4] - 2026-08-10\n\n- First tracked".to_string())
    );
    assert_eq!(
        for_startup_at(&path, RELEASES, version("0.1.4"), true)?,
        None
    );
    assert_eq!(
        for_startup_at(&path, RELEASES, version("0.1.5"), true)?,
        Some("## [0.1.5] - 2026-08-20\n\n- Newer".to_string())
    );
    assert_eq!(
        read_state(&path)?.last_seen_version.as_deref(),
        Some("0.1.5")
    );
    Ok(())
}

#[test]
fn fresh_install_skips_old_notes_but_tracks_later_updates() -> Result<()> {
    let root = TempDir::new();
    let path = root.state_path();

    assert_eq!(
        for_startup_at(&path, RELEASES, version("0.1.4"), false)?,
        None
    );
    assert_eq!(
        for_startup_at(&path, RELEASES, version("0.1.5"), true)?,
        Some("## [0.1.5] - 2026-08-20\n\n- Newer".to_string())
    );
    Ok(())
}

#[test]
fn malformed_or_duplicate_release_headings_are_rejected() {
    assert!(parse_changelog("## [1.2]\n\n- Invalid").is_err());
    assert!(parse_changelog("## [1.2.3]\n\n- One\n\n## [1.2.3]\n\n- Two").is_err());
}
