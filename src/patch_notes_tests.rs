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
## [0.1.6] - 2026-08-20\n\n- Latest\n\n\
## [0.1.5] - 2026-08-19\n\n- Newer\n\n\
## [0.1.4] - 2026-08-10\n\n- First tracked\n";

#[test]
fn released_notes_are_bounded_by_the_binary_and_ordered_for_scrollback() -> Result<()> {
    assert_eq!(
        notes_between(RELEASES, None, version("0.1.4"))?,
        Some("## [0.1.4] - 2026-08-10\n\n- First tracked".to_string())
    );
    assert_eq!(
        notes_between(RELEASES, Some(version("0.1.3")), version("0.1.6"))?,
        Some(
            "## [0.1.4] - 2026-08-10\n\n- First tracked\n\n\
             ## [0.1.5] - 2026-08-19\n\n- Newer\n\n\
             ## [0.1.6] - 2026-08-20\n\n- Latest"
                .to_string()
        )
    );
    Ok(())
}

#[test]
fn skipped_releases_are_indexed_and_acknowledged_only_after_presentation() -> Result<()> {
    let root = TempDir::new();
    let path = root.state_path();

    let first = for_startup_at(&path, RELEASES, version("0.1.4"), true)?;
    assert_eq!(
        first.notes(),
        Some("## [0.1.4] - 2026-08-10\n\n- First tracked")
    );
    assert!(!path.exists());
    first.mark_seen()?;

    let repeated = for_startup_at(&path, RELEASES, version("0.1.4"), true)?;
    assert_eq!(repeated.notes(), None);
    repeated.mark_seen()?;

    let skipped = for_startup_at(&path, RELEASES, version("0.1.6"), true)?;
    assert_eq!(
        skipped.notes(),
        Some(
            "## [0.1.5] - 2026-08-19\n\n- Newer\n\n\
             ## [0.1.6] - 2026-08-20\n\n- Latest\n\n\
             **Included releases:** `0.1.5` → `0.1.6`\n\n\
             Scroll up to review every release included in this update."
        )
    );
    assert_eq!(
        read_state(&path)?.last_seen_version.as_deref(),
        Some("0.1.4")
    );

    skipped.mark_seen()?;
    assert_eq!(
        read_state(&path)?.last_seen_version.as_deref(),
        Some("0.1.6")
    );
    Ok(())
}

#[test]
fn fresh_install_skips_old_notes_but_tracks_later_updates() -> Result<()> {
    let root = TempDir::new();
    let path = root.state_path();

    let fresh = for_startup_at(&path, RELEASES, version("0.1.4"), false)?;
    assert_eq!(fresh.notes(), None);
    assert!(!path.exists());
    fresh.mark_seen()?;

    let updated = for_startup_at(&path, RELEASES, version("0.1.5"), true)?;
    assert_eq!(updated.notes(), Some("## [0.1.5] - 2026-08-19\n\n- Newer"));
    updated.mark_seen()?;
    Ok(())
}

#[test]
fn delayed_acknowledgement_cannot_regress_a_newer_marker() -> Result<()> {
    let root = TempDir::new();
    let path = root.state_path();

    for_startup_at(&path, RELEASES, version("0.1.4"), false)?.mark_seen()?;
    let older = for_startup_at(&path, RELEASES, version("0.1.5"), true)?;
    let newer = for_startup_at(&path, RELEASES, version("0.1.6"), true)?;
    assert_eq!(older.notes(), Some("## [0.1.5] - 2026-08-19\n\n- Newer"));
    assert!(newer.notes().is_some_and(|notes| notes.contains("0.1.6")));

    newer.mark_seen()?;
    older.mark_seen()?;

    assert_eq!(
        read_state(&path)?.last_seen_version.as_deref(),
        Some("0.1.6")
    );
    Ok(())
}

#[test]
fn malformed_or_duplicate_release_headings_are_rejected() {
    assert!(parse_changelog("## [1.2]\n\n- Invalid").is_err());
    assert!(parse_changelog("## [1.2.3]\n\n- One\n\n## [1.2.3]\n\n- Two").is_err());
}
