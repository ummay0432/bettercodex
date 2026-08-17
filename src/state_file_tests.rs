use super::*;
use serde::Deserialize;
use std::path::PathBuf;

const MAX_TEST_STATE_BYTES: usize = 1024;

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "bettercodex-state-file-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default, Deserialize, Serialize)]
struct Document {
    value: u8,
}

fn read_document(path: &Path) -> Result<Document> {
    Ok(read_json(path, MAX_TEST_STATE_BYTES)?.unwrap_or_default())
}

#[test]
fn unchanged_update_does_not_materialize_a_state_file() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDirectory::new()?;
    let path = directory.join("state.json");
    let lock_path = companion_path(&path, ".lock")?;
    std::fs::write(&lock_path, b"")?;
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o666))?;

    update_json(&path, MAX_TEST_STATE_BYTES, read_document, |_| {
        Ok(StateChange::Unchanged)
    })?;

    assert!(!path.exists());
    assert_eq!(
        std::fs::metadata(lock_path)?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}

#[test]
fn read_repairs_broad_state_file_permissions() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDirectory::new()?;
    let path = directory.join("state.json");
    std::fs::write(&path, br#"{"value": 7}"#)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;

    let document = read_json::<Document>(&path, MAX_TEST_STATE_BYTES)?
        .expect("the state document should exist");

    assert_eq!(document.value, 7);
    assert_eq!(std::fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
    Ok(())
}

#[test]
fn read_rejects_a_symbolic_link_instead_of_following_it() -> Result<()> {
    use std::os::unix::fs::symlink;

    let directory = TempDirectory::new()?;
    let target = directory.join("target.json");
    let link = directory.join("state.json");
    std::fs::write(&target, br#"{"value": 7}"#)?;
    symlink(&target, &link)?;

    assert!(read_json::<Document>(&link, MAX_TEST_STATE_BYTES).is_err());
    Ok(())
}
