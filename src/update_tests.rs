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

    let revision = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
    assert_eq!(
        parse_release_tag(&format!("bcodex-v0.1.2-{revision}")).unwrap(),
        PublishedRelease {
            tag: format!("bcodex-v0.1.2-{revision}"),
            version: "0.1.2".to_string(),
            revision: revision.to_string(),
            assets: Vec::new(),
        }
    );
    for tag in [
        "bcodex-v0.1-abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "bcodex-v0.1.2-not-a-revision",
        "other-v0.1.2-abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "bcodex-v0.1.2-ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD",
    ] {
        assert!(parse_release_tag(tag).is_err(), "accepted {tag}");
    }
}

#[tokio::test]
async fn published_release_lookup_reports_both_exact_commits() {
    let current = "1111111111111111111111111111111111111111";
    let latest = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
    let curl = TemporaryProgram::new(&format!(
        "#!/bin/sh\n\
         printf '{{\"tag_name\":\"bcodex-v0.1.2-%s\",\"draft\":false,\"prerelease\":false}}\\n' '{latest}'\n"
    ));

    assert_eq!(
        check_for_release_update_with(
            curl.path.as_os_str(),
            "owner/project",
            current,
            Duration::from_secs(1),
        )
        .await,
        Some(AvailableUpdate {
            current_revision: current.to_string(),
            latest_revision: latest.to_string(),
        })
    );
    assert_eq!(
        check_for_release_update_with(
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
async fn failed_malformed_and_timed_out_release_lookups_are_silent() {
    let revision = "1111111111111111111111111111111111111111";
    for program in [
        "#!/bin/sh\nexit 1\n",
        "#!/bin/sh\nprintf '{\"tag_name\":\"not-a-release\",\"draft\":false,\"prerelease\":false}\\n'\n",
        "#!/bin/sh\nprintf '{\"tag_name\":\"bcodex-v0.1.2-1111111111111111111111111111111111111111\",\"draft\":true,\"prerelease\":false}\\n'\n",
    ] {
        let curl = TemporaryProgram::new(program);
        assert_eq!(
            check_for_release_update_with(
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
        check_for_release_update_with(
            slow.path.as_os_str(),
            "owner/project",
            revision,
            Duration::from_millis(20),
        )
        .await,
        None
    );
    assert_eq!(
        check_for_release_update_with(
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
fn updater_resolves_the_latest_release_with_asset_digests() {
    let revision = "2222222222222222222222222222222222222222";
    let tag = format!("bcodex-v0.1.2-{revision}");
    let digest = "a".repeat(64);
    let curl = TemporaryProgram::new(&format!(
        "#!/bin/sh\n\
         for argument do url=\"$argument\"; done\n\
         case \"$url\" in\n\
           'https://api.github.com/repos/owner/project/releases/latest') printf '%s\\n' '{{\"tag_name\":\"{tag}\",\"draft\":false,\"prerelease\":false,\"assets\":[{{\"name\":\"bcodex-x86_64-unknown-linux-gnu.zst\",\"size\":123,\"digest\":\"sha256:{digest}\"}}]}}' ;;\n\
           *) exit 2 ;;\n\
         esac\n"
    ));

    assert_eq!(
        resolve_published_release(curl.path.as_os_str(), "owner/project").unwrap(),
        PublishedRelease {
            tag,
            version: "0.1.2".to_string(),
            revision: revision.to_string(),
            assets: vec![ReleaseAsset {
                name: "bcodex-x86_64-unknown-linux-gnu.zst".to_string(),
                size: 123,
                sha256: digest,
            }],
        }
    );
}

#[test]
fn release_metadata_without_a_github_digest_cannot_be_installed() {
    let revision = "2222222222222222222222222222222222222222";
    let response = format!(
        "{{\"tag_name\":\"bcodex-v0.1.2-{revision}\",\"draft\":false,\"prerelease\":false,\"assets\":[{{\"name\":\"bcodex-x86_64-unknown-linux-gnu.zst\",\"size\":123,\"digest\":null}}]}}"
    );
    let release = parse_github_release(response.as_bytes()).unwrap();
    assert_eq!(release.assets[0].sha256, "");
    assert!(release.assets[0].validate().is_err());
}
