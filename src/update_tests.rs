use super::*;
use std::fs;

fn main_response(revision: &str) -> String {
    format!(
        "{{\"ref\":\"refs/heads/main\",\"object\":{{\"sha\":\"{revision}\",\"type\":\"commit\"}}}}"
    )
}

fn serve_once(
    status: u16,
    body: impl Into<Vec<u8>>,
    delay: Duration,
) -> (String, std::thread::JoinHandle<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let address = server.server_addr().to_ip().unwrap();
    let body = body.into();
    let task = std::thread::spawn(move || {
        let request = server.recv().unwrap();
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let _ = request.respond(
            tiny_http::Response::from_data(body).with_status_code(tiny_http::StatusCode(status)),
        );
    });
    (format!("http://{address}"), task)
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
    assert!(validate_repository("owner/public.repo").is_ok());
    assert!(validate_repository("owner").is_err());
    assert!(validate_repository("owner/repo/extra").is_err());
    assert!(validate_repository("owner/repo?ref=other").is_err());
    assert!(is_build_input_hash(&"a".repeat(64)));
    assert!(is_build_input_hash(&"F".repeat(64)));
    assert!(!is_build_input_hash(&"a".repeat(63)));
    assert!(!is_build_input_hash(&"g".repeat(64)));
    assert_eq!(build_input_hash(), None);
}

#[test]
fn source_revision_staging_replaces_one_framed_marker() {
    let revision = "ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD";
    let mut image = b"binary prefix".to_vec();
    let marker_offset = image.len();
    image.extend_from_slice(&SOURCE_REVISION_METADATA);
    image.extend_from_slice(b"binary suffix");

    patch_source_revision(&mut image, revision).unwrap();

    assert_eq!(
        &image[marker_offset + SOURCE_REVISION_OFFSET
            ..marker_offset + SOURCE_REVISION_OFFSET + SOURCE_REVISION_LENGTH],
        revision.as_bytes()
    );
    assert!(patch_source_revision(&mut image, revision).is_err());
    assert!(patch_source_revision(&mut [0; 128], revision).is_err());
    let mut marker = SOURCE_REVISION_METADATA;
    assert!(patch_source_revision(&mut marker, "invalid").is_err());

    let mut duplicate = SOURCE_REVISION_METADATA.repeat(2);
    assert!(patch_source_revision(&mut duplicate, revision).is_err());
}

#[test]
fn current_binary_contains_one_source_revision_record() {
    let executable = std::env::current_exe().unwrap();
    let image = fs::read(executable).unwrap();
    assert_eq!(
        memmem::find_iter(&image, SOURCE_REVISION_METADATA.as_slice()).count(),
        1
    );
    assert_eq!(source_revision(), None);
}

#[test]
fn accepts_only_the_main_commit_ref_response() {
    let revision = "ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD";
    assert_eq!(
        parse_github_main_revision(main_response(revision).as_bytes()).unwrap(),
        revision.to_ascii_lowercase()
    );
    for response in [
        "{}",
        "{\"ref\":\"refs/heads/other\",\"object\":{\"sha\":\"1111111111111111111111111111111111111111\",\"type\":\"commit\"}}",
        "{\"ref\":\"refs/heads/main\",\"object\":{\"sha\":\"1111111111111111111111111111111111111111\",\"type\":\"tag\"}}",
        "{\"ref\":\"refs/heads/main\",\"object\":{\"sha\":\"not-a-commit\",\"type\":\"commit\"}}",
    ] {
        assert!(parse_github_main_revision(response.as_bytes()).is_err());
    }
}

#[tokio::test]
async fn bounded_http_lookup_reports_different_revisions_and_ignores_equal_ones() {
    let current = "1111111111111111111111111111111111111111";
    let latest = "ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD";
    let (url, server) = serve_once(200, main_response(latest), Duration::from_millis(0));
    assert_eq!(
        check_for_source_update_at(&url, current, Duration::from_secs(1)).await,
        Some(AvailableUpdate {
            current_revision: current.to_string(),
            latest_revision: latest.to_ascii_lowercase(),
        })
    );
    server.join().unwrap();

    let (url, server) = serve_once(200, main_response(latest), Duration::from_millis(0));
    assert_eq!(
        check_for_source_update_at(&url, latest, Duration::from_secs(1)).await,
        None
    );
    server.join().unwrap();
}

#[tokio::test]
async fn malformed_failed_oversized_and_timed_out_lookups_are_silent() {
    let revision = "1111111111111111111111111111111111111111";
    for (status, body) in [
        (500, b"failure".to_vec()),
        (200, br#"{"ref":"refs/heads/main"}"#.to_vec()),
        (200, vec![b'x'; MAX_INSTALLER_BYTES + 1]),
    ] {
        let (url, server) = serve_once(status, body, Duration::from_millis(0));
        assert_eq!(
            check_for_source_update_at(&url, revision, Duration::from_secs(1)).await,
            None
        );
        server.join().unwrap();
    }

    let (url, server) = serve_once(200, main_response(revision), Duration::from_millis(100));
    assert_eq!(
        check_for_source_update_at(&url, revision, Duration::from_millis(10)).await,
        None
    );
    server.join().unwrap();
    assert_eq!(
        check_for_source_update_with("invalid repository", revision, Duration::from_millis(10),)
            .await,
        None
    );
}

#[cfg(unix)]
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

#[cfg(windows)]
#[test]
fn updater_targets_the_running_windows_binary_directory_unless_configured() {
    assert_eq!(
        update_install_dir(Path::new(r"C:\Programs\bettercodex\bcodex.exe"), None).unwrap(),
        PathBuf::from(r"C:\Programs\bettercodex")
    );
    assert_eq!(
        update_install_dir(
            Path::new(r"C:\Programs\bettercodex\bcodex.exe"),
            Some(OsStr::new(r"D:\Custom bettercodex")),
        )
        .unwrap(),
        PathBuf::from(r"D:\Custom bettercodex")
    );
    assert!(
        update_install_dir(
            Path::new(r"C:\Programs\bettercodex\bcodex.exe"),
            Some(OsStr::new("relative")),
        )
        .is_err()
    );
}

#[test]
fn updater_selects_the_target_native_installer_at_the_pinned_revision() {
    let revision = "2222222222222222222222222222222222222222";
    assert_eq!(
        installer_url("owner/project", revision),
        format!(
            "https://raw.githubusercontent.com/owner/project/{revision}/{}",
            installer_path()
        )
    );
    #[cfg(unix)]
    assert_eq!(installer_path(), "scripts/install.sh");
    #[cfg(windows)]
    assert_eq!(installer_path(), "scripts/install.ps1");
}

#[cfg(unix)]
#[test]
fn updater_passes_only_the_pinned_source_install_contract() {
    let revision = "2222222222222222222222222222222222222222";
    run_installer_script(
        b"#!/bin/sh\ntest \"$BCODEX_INSTALL_DIR\" = '/tmp/custom bettercodex'\n\
          test \"$BCODEX_REPOSITORY\" = owner/project\n\
          test \"$BCODEX_INSTALL_REVISION\" = 2222222222222222222222222222222222222222\n\
          test -z \"${BCODEX_INSTALL_RELEASE_TAG:-}\"\n\
          test -z \"${BCODEX_INSTALL_VERSION:-}\"\n",
        Path::new("/tmp/custom bettercodex"),
        "owner/project",
        revision,
    )
    .unwrap();
    assert!(
        run_installer_script(
            b"#!/bin/sh\nexit 7\n",
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
            b"#!/bin/sh\nexit 0\n",
            Path::new("/tmp/custom bettercodex"),
            "owner/project",
            "not-a-revision",
        )
        .unwrap_err()
        .to_string()
        .contains("invalid source revision")
    );
}

#[cfg(windows)]
#[test]
fn updater_runs_the_pinned_powershell_installer_from_a_file() {
    let revision = "2222222222222222222222222222222222222222";
    let install_dir = std::env::temp_dir().join("bettercodex update test");
    let script = format!(
        "#Requires -Version 5.1\nif ($env:BCODEX_INSTALL_DIR -cne '{}') {{ exit 2 }}\nif ($env:BCODEX_REPOSITORY -cne 'owner/project') {{ exit 3 }}\nif ($env:BCODEX_INSTALL_REVISION -cne '{revision}') {{ exit 4 }}\nif (-not $env:BCODEX_UPDATE_PARENT_PID) {{ exit 5 }}\nexit 0\n",
        install_dir.display()
    );
    run_installer_script(script.as_bytes(), &install_dir, "owner/project", revision).unwrap();
}

#[cfg(windows)]
#[test]
fn temporary_update_script_is_removed_on_drop() {
    let path = std::env::temp_dir().join(format!(
        "bettercodex-update-cleanup-test-{}-{}.ps1",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::write(&path, b"test").unwrap();
    let script = TemporaryUpdateScript::new(path.clone());

    drop(script);

    let removed = !path.exists();
    let _ = fs::remove_file(path);
    assert!(removed);
}
