use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("bettercodex-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct TemporaryProgram {
    _root: TemporaryDirectory,
    path: PathBuf,
}

impl TemporaryProgram {
    fn new(contents: &str) -> Self {
        let root = TemporaryDirectory::new("update");
        let path = root.path.join("curl");
        fs::write(&path, contents).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        Self { _root: root, path }
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
async fn public_revision_lookup_reports_both_exact_commits() {
    let current = "1111111111111111111111111111111111111111";
    let latest = "ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD";
    let curl = TemporaryProgram::new(&format!(
        "#!/bin/sh\n\
         printf '{{\"sha\":\"%s\"}}\\n' '{latest}'\n"
    ));

    assert_eq!(
        check_for_source_update_with(
            curl.path.as_os_str(),
            "owner/project",
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
            curl.path.as_os_str(),
            "owner/project",
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
        "#!/bin/sh\nprintf '{\"sha\":\"not-a-commit\"}\\n'\n",
    ] {
        let curl = TemporaryProgram::new(program);
        assert_eq!(
            check_for_source_update_with(
                curl.path.as_os_str(),
                "owner/project",
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
            "owner/project",
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
    let curl = TemporaryProgram::new(&format!(
        "#!/bin/sh\n\
         for argument do url=\"$argument\"; done\n\
         case \"$url\" in\n\
           'https://api.github.com/repos/owner/project/commits/main') printf '{{\"sha\":\"%s\"}}\\n' '{revision}' ;;\n\
           'https://raw.githubusercontent.com/owner/project/{revision}/scripts/install.sh') printf '%s\\n' '#!/bin/sh' 'exit 0' ;;\n\
           *) exit 2 ;;\n\
         esac\n"
    ));

    assert_eq!(
        resolve_source_revision(curl.path.as_os_str(), "owner/project").unwrap(),
        revision
    );
    assert_eq!(
        fetch_installer(
            curl.path.as_os_str(),
            "owner/project",
            revision,
            MAX_INSTALLER_BYTES,
        )
        .unwrap(),
        b"#!/bin/sh\nexit 0\n"
    );
    assert!(fetch_installer(curl.path.as_os_str(), "owner/project", revision, 8,).is_err());
}

#[test]
fn updater_passes_the_target_directory_repository_and_revision_to_the_installer() {
    let revision = "2222222222222222222222222222222222222222";
    run_installer_script(
        b"test \"$BCODEX_INSTALL_DIR\" = '/tmp/custom bettercodex'\n\
          test \"$BCODEX_REPOSITORY\" = owner/project\n\
          test \"$BCODEX_INSTALL_REVISION\" = 2222222222222222222222222222222222222222\n",
        OsStr::new("/bin/sh"),
        Path::new("/tmp/custom bettercodex"),
        "owner/project",
        revision,
    )
    .unwrap();
    assert!(
        run_installer_script(
            b"exit 7\n",
            OsStr::new("/bin/sh"),
            Path::new("/tmp/custom bettercodex"),
            "owner/project",
            revision,
        )
        .unwrap_err()
        .to_string()
        .contains("exit status: 7")
    );
    assert!(
        run_installer_script(
            b"exit 0\n",
            OsStr::new("/bin/sh"),
            Path::new("/tmp/custom bettercodex"),
            "owner/project",
            "not-a-revision",
        )
        .unwrap_err()
        .to_string()
        .contains("invalid source revision")
    );
}
