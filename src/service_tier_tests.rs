use super::*;

struct DirectoryCleanup(std::path::PathBuf);

impl Drop for DirectoryCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn saved_preference_restores_the_last_fast_toggle() -> Result<()> {
    let directory = std::env::temp_dir().join(format!(
        "bettercodex-service-tier-settings-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&directory)?;
    let _cleanup = DirectoryCleanup(directory.clone());
    let path = directory.join(SETTINGS_FILE);

    assert_eq!(read_settings(&path)?.service_tier, ServiceTier::Standard);

    save_default_to(&path, ServiceTier::Fast)?;
    assert_eq!(read_settings(&path)?.service_tier, ServiceTier::Fast);

    save_default_to(&path, ServiceTier::Standard)?;
    assert_eq!(read_settings(&path)?.service_tier, ServiceTier::Standard);

    Ok(())
}
