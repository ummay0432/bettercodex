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
    let directory = TempDirectory::new()?;
    let path = directory.join("state.json");

    update_json(&path, MAX_TEST_STATE_BYTES, read_document, |_| {
        Ok(StateChange::Unchanged)
    })?;

    assert!(!path.exists());
    Ok(())
}

#[cfg(unix)]
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
