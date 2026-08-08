use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
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
fn accepts_only_full_source_revisions_and_safe_repository_names() {
    assert!(is_source_revision(
        "1111111111111111111111111111111111111111"
    ));
    assert!(is_source_revision(
        "ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD"
    ));
    assert!(!is_source_revision(
        "111111111111111111111111111111111111111"
    ));
    assert!(!is_source_revision(
        "111111111111111111111111111111111111111g"
    ));
    assert!(validate_repository("owner/private.repo").is_ok());
    assert!(validate_repository("owner").is_err());
    assert!(validate_repository("owner/repo/extra").is_err());
    assert!(validate_repository("owner/repo?ref=other").is_err());
}

#[tokio::test]
async fn authenticated_revision_lookup_reports_both_exact_commits() {
    let current = "1111111111111111111111111111111111111111";
    let latest = "ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD";
    let gh = TemporaryProgram::new(&format!(
        "#!/bin/sh\n\
         test \"$*\" = \"api repos/owner/private/commits/main --jq .sha\" || exit 2\n\
         printf '%s\\n' '{latest}'\n"
    ));

    assert_eq!(
        check_for_source_update_with(
            gh.path.as_os_str(),
            "owner/private",
            current,
            Duration::from_secs(1),
        )
        .await,
        Some(AvailableUpdate {
            current_revision: current.to_string(),
            latest_revision: latest.to_ascii_lowercase(),
        })
    );
    assert_eq!(
        check_for_source_update_with(
            gh.path.as_os_str(),
            "owner/private",
            latest,
            Duration::from_secs(1),
        )
        .await,
        None
    );
}

#[tokio::test]
async fn failed_malformed_and_timed_out_revision_lookups_are_silent() {
    let revision = "1111111111111111111111111111111111111111";
    for program in [
        "#!/bin/sh\nexit 1\n",
        "#!/bin/sh\nprintf 'not-a-commit\\n'\n",
    ] {
        let gh = TemporaryProgram::new(program);
        assert_eq!(
            check_for_source_update_with(
                gh.path.as_os_str(),
                "owner/private",
                revision,
                Duration::from_secs(1),
            )
            .await,
            None
        );
    }

    let slow = TemporaryProgram::new("#!/bin/sh\nsleep 2\n");
    assert_eq!(
        check_for_source_update_with(
            slow.path.as_os_str(),
            "owner/private",
            revision,
            Duration::from_millis(20),
        )
        .await,
        None
    );
    assert_eq!(
        check_for_source_update_with(
            slow.path.as_os_str(),
            "invalid repository",
            revision,
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
    assert!(
        update_install_dir(
            Path::new("/opt/bettercodex/bin/bcodex"),
            Some(OsStr::new("relative")),
        )
        .is_err()
    );
}

#[test]
fn updater_resolves_main_and_fetches_the_installer_from_that_revision() {
    let revision = "2222222222222222222222222222222222222222";
    let gh = TemporaryProgram::new(&format!(
        "#!/bin/sh\n\
         case \"$*\" in\n\
           'api repos/owner/private/commits/main --jq .sha') printf '%s\\n' '{revision}' ;;\n\
           'api -H Accept: application/vnd.github.raw+json repos/owner/private/contents/scripts/install.sh?ref={revision}') printf '%s\\n' '#!/bin/sh' 'exit 0' ;;\n\
           *) exit 2 ;;\n\
         esac\n"
    ));

    assert_eq!(
        resolve_source_revision(gh.path.as_os_str(), "owner/private").unwrap(),
        revision
    );
    assert_eq!(
        fetch_installer(
            gh.path.as_os_str(),
            "owner/private",
            revision,
            MAX_INSTALLER_BYTES,
        )
        .unwrap(),
        b"#!/bin/sh\nexit 0\n"
    );
    assert!(fetch_installer(gh.path.as_os_str(), "owner/private", revision, 8,).is_err());
}

#[test]
fn updater_passes_the_target_directory_and_repository_to_the_installer() {
    run_installer_script(
        b"test \"$BCODEX_INSTALL_DIR\" = '/tmp/custom bettercodex'\n\
          test \"$BCODEX_REPOSITORY\" = owner/private\n",
        OsStr::new("/bin/sh"),
        Path::new("/tmp/custom bettercodex"),
        "owner/private",
    )
    .unwrap();
    assert!(
        run_installer_script(
            b"exit 7\n",
            OsStr::new("/bin/sh"),
            Path::new("/tmp/custom bettercodex"),
            "owner/private",
        )
        .unwrap_err()
        .to_string()
        .contains("exit status: 7")
    );
}

#[test]
fn legacy_cleanup_removes_only_retired_directories() {
    let root = std::env::temp_dir().join(format!("bettercodex-cache-{}", Uuid::new_v4()));
    fs::create_dir_all(root.join("build/target/release")).unwrap();
    fs::create_dir_all(root.join("tmp/source")).unwrap();
    fs::create_dir_all(root.join("rusty-v8-150.4.0-host")).unwrap();
    fs::write(root.join("keep"), "operator data").unwrap();

    cleanup_legacy_updater_cache_in(&root).unwrap();

    assert!(!root.join("build").exists());
    assert!(!root.join("tmp").exists());
    assert!(root.join("rusty-v8-150.4.0-host").is_dir());
    assert_eq!(
        fs::read_to_string(root.join("keep")).unwrap(),
        "operator data"
    );
    fs::remove_dir_all(root).unwrap();
}
