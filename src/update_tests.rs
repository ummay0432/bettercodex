use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

struct TemporaryProgram {
    root: PathBuf,
    path: PathBuf,
}

impl TemporaryProgram {
    fn new(contents: &str) -> Self {
        let root = std::env::temp_dir().join(format!("bettercodex-update-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("gh");
        fs::write(&path, contents).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        Self { root, path }
    }
}

impl Drop for TemporaryProgram {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn compares_only_stable_three_component_versions() {
    assert_eq!(is_newer("0.1.1", "0.1.0"), Some(true));
    assert_eq!(is_newer("0.1.0", "0.1.0"), Some(false));
    assert_eq!(is_newer("0.0.9", "0.1.0"), Some(false));
    assert_eq!(is_newer("1.0.0", "0.99.99"), Some(true));
    assert_eq!(is_newer("1.0", "0.1.0"), None);
    assert_eq!(is_newer("1.0.0-beta.1", "0.1.0"), None);
    assert_eq!(is_newer("1.0.0.1", "0.1.0"), None);
}

#[tokio::test]
async fn authenticated_release_lookup_returns_only_a_newer_version() {
    let gh = TemporaryProgram::new(
        "#!/bin/sh\n\
         test \"$*\" = \"release view --repo owner/private --json tagName --jq .tagName\" || exit 2\n\
         printf 'v1.2.3\\n'\n",
    );

    assert_eq!(
        check_for_update_with(
            gh.path.as_os_str(),
            "owner/private",
            "1.2.2",
            Duration::from_secs(1),
        )
        .await,
        Some(AvailableUpdate {
            current_version: "1.2.2".to_string(),
            latest_version: "1.2.3".to_string(),
        })
    );
    assert_eq!(
        check_for_update_with(
            gh.path.as_os_str(),
            "owner/private",
            "1.2.3",
            Duration::from_secs(1),
        )
        .await,
        None
    );
}

#[tokio::test]
async fn lookup_failures_and_unexpected_tags_are_silent() {
    let failure = TemporaryProgram::new("#!/bin/sh\nexit 1\n");
    assert_eq!(
        check_for_update_with(
            failure.path.as_os_str(),
            "owner/private",
            "1.2.2",
            Duration::from_secs(1),
        )
        .await,
        None
    );

    let invalid = TemporaryProgram::new("#!/bin/sh\nprintf 'release-1.2.3\\n'\n");
    assert_eq!(
        check_for_update_with(
            invalid.path.as_os_str(),
            "owner/private",
            "1.2.2",
            Duration::from_secs(1),
        )
        .await,
        None
    );
}

#[test]
fn updater_targets_the_running_binary_directory_unless_configured() {
    assert_eq!(
        update_install_dir(Path::new("/opt/bettercodex/bin/bcodex"), None).unwrap(),
        PathBuf::from("/opt/bettercodex/bin")
    );
    assert_eq!(
        update_install_dir(
            Path::new("/opt/bettercodex/bin/bcodex"),
            Some(OsStr::new("/srv/custom bin")),
        )
        .unwrap(),
        PathBuf::from("/srv/custom bin")
    );
    assert!(update_install_dir(Path::new("bcodex"), None).is_err());
}

#[test]
fn updater_pipes_the_installer_to_a_shell_with_release_and_location() {
    run_installer_script(
        b"test \"$BCODEX_RELEASE\" = latest\n\
          test \"$BCODEX_INSTALL_DIR\" = '/tmp/custom bettercodex'\n",
        OsStr::new("/bin/sh"),
        OsStr::new("/tmp/custom bettercodex"),
    )
    .unwrap();
    assert!(
        run_installer_script(
            b"exit 7\n",
            OsStr::new("/bin/sh"),
            OsStr::new("/tmp/custom bettercodex"),
        )
        .unwrap_err()
        .to_string()
        .contains("exit status: 7")
    );
}
