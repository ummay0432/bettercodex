use super::*;
use serde_json::Value;
use serde_json::json;
use std::fs;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::time::SystemTime;

fn release_tag_fixture(version: &str, revision: char) -> String {
    format!("bcodex-v{version}-{}", revision.to_string().repeat(40))
}

fn release_document(version: &str, revision: char) -> Value {
    let revision = revision.to_string().repeat(40);
    json!({
        "tag_name": format!("bcodex-v{version}-{revision}"),
        "target_commitish": revision,
        "draft": false,
        "prerelease": false,
        "immutable": true,
        "assets": [{
            "name": RELEASE_ASSET_NAME,
            "state": "uploaded",
            "size": 1024,
            "digest": format!("sha256:{}", "a".repeat(64)),
        }],
    })
}

fn release_response(version: &str, revision: char) -> Vec<u8> {
    serde_json::to_vec(&release_document(version, revision)).unwrap()
}

fn release_fixture(version: &str, revision: char) -> Release {
    parse_latest_release(&release_response(version, revision)).unwrap()
}

fn installer_script(body: &str) -> Vec<u8> {
    let mut script = INSTALLER_PREFIX.to_vec();
    script.extend_from_slice(body.as_bytes());
    script
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

fn serve_redirect_once(location: &str) -> (String, std::thread::JoinHandle<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let address = server.server_addr().to_ip().unwrap();
    let location = tiny_http::Header::from_bytes("Location", location).unwrap();
    let task = std::thread::spawn(move || {
        let request = server.recv().unwrap();
        let _ = request.respond(tiny_http::Response::empty(302).with_header(location));
    });
    (format!("http://{address}"), task)
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "bettercodex-update-tests.{}.{}.{}",
            std::process::id(),
            nonce,
            id
        ));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn one_build_policy_controls_background_and_explicit_updates() {
    assert_eq!(classify_build(true, false), BuildKind::DebugSource);
    assert_eq!(classify_build(true, true), BuildKind::DebugSource);
    assert_eq!(classify_build(false, false), BuildKind::ReleaseSource);
    assert_eq!(classify_build(false, true), BuildKind::Published);

    assert!(!BuildKind::DebugSource.checks_for_updates());
    assert!(!BuildKind::DebugSource.supports_explicit_update());
    assert!(BuildKind::ReleaseSource.checks_for_updates());
    assert!(BuildKind::ReleaseSource.supports_explicit_update());
    assert!(BuildKind::Published.checks_for_updates());
    assert!(BuildKind::Published.supports_explicit_update());
    assert!(
        BuildKind::DebugSource
            .update_command(
                Path::new("/tmp/bcodex"),
                Path::new("/tmp"),
                DEFAULT_REPOSITORY,
            )
            .is_none()
    );
    for (selected_version, revision) in [("1.2.2", '1'), ("1.2.3", '2'), ("1.3.0", '3')] {
        let selected = release_fixture(selected_version, revision);
        assert_eq!(
            explicit_update_decision(BuildKind::ReleaseSource, false, "1.2.3", None, &selected,)
                .unwrap(),
            ExplicitUpdateDecision::Install
        );
        assert_eq!(
            explicit_update_decision(BuildKind::Published, true, "1.2.3", None, &selected).unwrap(),
            ExplicitUpdateDecision::Install
        );
    }

    let newer = release_fixture("1.3.0", '3');
    assert_eq!(
        explicit_update_decision(BuildKind::Published, false, "1.2.3", None, &newer).unwrap(),
        ExplicitUpdateDecision::Install
    );
    assert_eq!(
        explicit_update_decision(
            BuildKind::Published,
            false,
            "1.3.0",
            Some(&newer.tag),
            &newer,
        )
        .unwrap(),
        ExplicitUpdateDecision::AlreadyLatest
    );
    assert_eq!(
        explicit_update_decision(
            BuildKind::Published,
            true,
            "1.3.0",
            Some(&newer.tag),
            &newer,
        )
        .unwrap(),
        ExplicitUpdateDecision::AlreadySelected
    );

    let older = release_fixture("1.2.2", '1');
    assert_eq!(
        explicit_update_decision(BuildKind::Published, false, "1.2.3", None, &older).unwrap(),
        ExplicitUpdateDecision::CurrentIsNewer
    );

    let equal_different_revision = release_fixture("1.2.3", '4');
    assert_eq!(
        explicit_update_decision(
            BuildKind::Published,
            false,
            "1.2.3",
            None,
            &equal_different_revision,
        )
        .unwrap(),
        ExplicitUpdateDecision::Install
    );
    assert!(
        explicit_update_decision(
            BuildKind::Published,
            false,
            "1.2.3",
            Some(&release_tag_fixture("1.2.3", '5')),
            &equal_different_revision,
        )
        .unwrap_err()
        .to_string()
        .contains("reuses version")
    );
    assert!(
        explicit_update_decision(BuildKind::DebugSource, false, "1.2.3", None, &newer).is_err()
    );

    let matching_tag = release_tag_fixture("1.2.3", '1');
    assert_eq!(
        valid_embedded_release_tag(false, Some(&matching_tag), "1.2.3"),
        Some(matching_tag.as_str())
    );
    assert_eq!(
        valid_embedded_release_tag(false, Some(&matching_tag), "1.2.4"),
        None
    );
    assert_eq!(
        valid_embedded_release_tag(false, Some("invalid"), "1.2.3"),
        None
    );
    assert_eq!(
        valid_embedded_release_tag(true, Some(&matching_tag), "1.2.3"),
        None
    );
    assert_eq!(
        BuildKind::current(),
        classify_build(cfg!(debug_assertions), release_tag().is_some())
    );
    if cfg!(debug_assertions) {
        assert!(background_update_check().is_none());
        assert_eq!(release_tag(), None);
        assert_eq!(source_revision(), None);
    }
}

#[test]
fn accepts_only_strict_release_tags_versions_digests_and_repository_names() {
    let tag = release_tag_fixture("1.2.3", 'a');
    let parsed = parse_release_tag(&tag).unwrap();
    assert_eq!(parsed.version, "1.2.3");
    assert_eq!(parsed.revision, "a".repeat(40));

    for tag in [
        "v1.2.3-1111111111111111111111111111111111111111",
        "bcodex-v1.2-1111111111111111111111111111111111111111",
        "bcodex-v01.2.3-1111111111111111111111111111111111111111",
        "bcodex-v1.2.3-not-a-revision",
        "bcodex-v1.2.3-beta-1111111111111111111111111111111111111111",
        "bcodex-v1.2.3-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert!(parse_release_tag(tag).is_err(), "accepted {tag}");
    }

    assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
    assert_eq!(
        parse_version("18446744073709551615.0.0"),
        Some((u64::MAX, 0, 0))
    );
    for version in [
        "01.2.3",
        "1.02.3",
        "1.2.03",
        "1.2",
        "1.2.3.4",
        "1.2.3-beta",
        "18446744073709551616.0.0",
    ] {
        assert_eq!(parse_version(version), None, "accepted {version}");
    }
    assert_eq!(compare_versions("1.3.0", "1.2.9"), Some(Ordering::Greater));
    assert_eq!(compare_versions("1.3.0", "1.3.0"), Some(Ordering::Equal));
    assert_eq!(compare_versions("1.2.9", "1.3.0"), Some(Ordering::Less));
    assert_eq!(compare_versions("invalid", "1.3.0"), None);

    let digest = format!("sha256:{}", "a".repeat(64));
    assert_eq!(parse_sha256_digest(&digest), Some(&digest[7..]));
    assert!(parse_sha256_digest(&format!("sha256:{}", "a".repeat(63))).is_none());
    assert!(parse_sha256_digest(&format!("sha256:{}", "A".repeat(64))).is_none());
    assert!(parse_sha256_digest(&format!("sha512:{}", "a".repeat(64))).is_none());

    assert!(validate_repository("owner/public.repo").is_ok());
    for repository in [
        "owner",
        "owner/repo/extra",
        "owner/repo?ref=other",
        "./repo",
        "../repo",
        "owner/.",
        "owner/..",
    ] {
        assert!(
            validate_repository(repository).is_err(),
            "accepted {repository}"
        );
    }

    assert_eq!(
        configured_repository_from(None).unwrap(),
        ConfiguredRepository {
            name: DEFAULT_REPOSITORY.to_string(),
            overridden: false,
        }
    );
    assert_eq!(
        configured_repository_from(Some(OsString::new())).unwrap(),
        ConfiguredRepository {
            name: DEFAULT_REPOSITORY.to_string(),
            overridden: false,
        }
    );
    assert_eq!(
        configured_repository_from(Some(OsString::from("owner/project"))).unwrap(),
        ConfiguredRepository {
            name: "owner/project".to_string(),
            overridden: true,
        }
    );
    assert!(
        configured_repository_from(Some(OsString::from_vec(b"owner/proj\xffct".to_vec()))).is_err()
    );
}

#[test]
fn latest_release_requires_an_immutable_exact_target_and_one_complete_native_asset() {
    let release = parse_latest_release(&release_response("1.2.3", '1')).unwrap();
    assert_eq!(
        release,
        Release {
            tag: release_tag_fixture("1.2.3", '1'),
            version: "1.2.3".to_string(),
            revision: "1".repeat(40),
            asset: ReleaseAsset {
                size: 1024,
                sha256: "a".repeat(64),
            },
        }
    );

    let base = release_document("1.2.3", '1');
    let mut invalid = Vec::new();

    let mut value = base.clone();
    value["draft"] = json!(true);
    invalid.push(value);
    let mut value = base.clone();
    value["prerelease"] = json!(true);
    invalid.push(value);
    let mut value = base.clone();
    value["immutable"] = json!(false);
    invalid.push(value);
    let mut value = base.clone();
    value.as_object_mut().unwrap().remove("immutable");
    invalid.push(value);
    let mut value = base.clone();
    value["target_commitish"] = json!("2".repeat(40));
    invalid.push(value);
    let mut value = base.clone();
    value["tag_name"] = json!(release_tag_fixture("1.2", '1'));
    invalid.push(value);
    let mut value = base.clone();
    value["assets"][0]["name"] = json!("wrong-asset.gz");
    invalid.push(value);
    let mut value = base.clone();
    value["assets"][0]["state"] = json!("new");
    invalid.push(value);
    let mut value = base.clone();
    value["assets"][0]["size"] = json!(0);
    invalid.push(value);
    let mut value = base.clone();
    value["assets"][0]["size"] = json!(MAX_RELEASE_ASSET_BYTES + 1);
    invalid.push(value);
    let mut value = base.clone();
    value["assets"][0]["digest"] = json!(format!("sha256:{}", "A".repeat(64)));
    invalid.push(value);
    let mut value = base;
    let duplicate = value["assets"][0].clone();
    value["assets"].as_array_mut().unwrap().push(duplicate);
    invalid.push(value);

    for response in invalid {
        assert!(
            parse_latest_release(&serde_json::to_vec(&response).unwrap()).is_err(),
            "accepted {response}"
        );
    }
    assert!(parse_latest_release(b"{}").is_err());
    assert!(parse_latest_release(b"not json").is_err());
}

#[tokio::test]
async fn bounded_lookup_reports_only_newer_actionable_release_versions() {
    let current_version = "1.2.3";
    let (url, server) = serve_once(
        200,
        release_response("1.3.0", '2'),
        Duration::from_millis(0),
    );
    assert_eq!(
        check_for_release_update_at(&url, current_version, Duration::from_secs(1)).await,
        Some("1.3.0".to_string())
    );
    server.join().unwrap();

    for version in ["1.2.3", "1.2.2"] {
        let (url, server) = serve_once(
            200,
            release_response(version, '2'),
            Duration::from_millis(0),
        );
        assert_eq!(
            check_for_release_update_at(&url, current_version, Duration::from_secs(1)).await,
            None
        );
        server.join().unwrap();
    }
}

#[tokio::test]
async fn malformed_failed_oversized_and_timed_out_lookups_are_silent() {
    let current_version = "1.2.3";
    for (status, body) in [
        (500, b"failure".to_vec()),
        (200, br#"{"tag_name":"invalid"}"#.to_vec()),
        (200, vec![b'x'; MAX_RELEASE_METADATA_BYTES + 1]),
    ] {
        let (url, server) = serve_once(status, body, Duration::from_millis(0));
        assert_eq!(
            check_for_release_update_at(&url, current_version, Duration::from_secs(1)).await,
            None
        );
        server.join().unwrap();
    }

    let (url, server) = serve_once(
        200,
        release_response("1.3.0", '2'),
        Duration::from_millis(100),
    );
    assert_eq!(
        check_for_release_update_at(&url, current_version, Duration::from_millis(10)).await,
        None
    );
    server.join().unwrap();
    assert_eq!(
        check_for_release_update_with(
            "invalid repository",
            current_version,
            Duration::from_millis(10),
        )
        .await,
        None
    );
}

#[tokio::test]
async fn installer_download_is_bounded_and_requires_the_exact_prefix() {
    assert!(valid_installer_prefix(include_bytes!(
        "../scripts/install.sh"
    )));

    let client = update_client(Duration::from_secs(1)).unwrap();
    let valid = installer_script("exit 0\n");
    let (url, server) = serve_once(200, valid.clone(), Duration::ZERO);
    assert_eq!(
        fetch_installer_at(&client, &url, MAX_INSTALLER_BYTES)
            .await
            .unwrap(),
        valid
    );
    server.join().unwrap();

    for (status, body) in [
        (500, b"failure".to_vec()),
        (200, b"#!/bin/sh\nexit 0\n".to_vec()),
        (200, vec![b'x'; MAX_INSTALLER_BYTES + 1]),
    ] {
        let (url, server) = serve_once(status, body, Duration::ZERO);
        assert!(
            fetch_installer_at(&client, &url, MAX_INSTALLER_BYTES)
                .await
                .is_err()
        );
        server.join().unwrap();
    }

    let (url, server) = serve_redirect_once("http://127.0.0.1:9/installer");
    assert!(
        fetch_installer_at(&client, &url, MAX_INSTALLER_BYTES)
            .await
            .is_err()
    );
    server.join().unwrap();

    let client = update_client(Duration::from_millis(10)).unwrap();
    let (url, server) = serve_once(
        200,
        installer_script("exit 0\n"),
        Duration::from_millis(100),
    );
    assert!(
        fetch_installer_at(&client, &url, MAX_INSTALLER_BYTES)
            .await
            .is_err()
    );
    server.join().unwrap();
}

#[test]
fn updater_keeps_cargo_artifacts_outside_the_published_install_channel() {
    let published_executable = Path::new("/opt/bettercodex/bin/bcodex");
    let published_dir = Path::new("/opt/bettercodex/bin");
    assert_eq!(
        update_install_dir(BuildKind::Published, published_executable, None).unwrap(),
        published_dir
    );
    assert_eq!(
        update_install_dir(
            BuildKind::Published,
            published_executable,
            Some(published_dir),
        )
        .unwrap(),
        published_dir
    );
    assert!(
        update_install_dir(
            BuildKind::Published,
            published_executable,
            Some(Path::new("/srv/custom bin")),
        )
        .is_err()
    );
    assert!(
        update_install_dir(
            BuildKind::Published,
            Path::new("/opt/bettercodex/renamed"),
            None,
        )
        .is_err()
    );
    assert!(
        update_install_dir(
            BuildKind::Published,
            Path::new("/opt/bettercodex/target/release/bcodex"),
            None,
        )
        .is_err()
    );

    let installed_source_executable = Path::new("/home/user/.local/bin/bcodex");
    let installed_source_dir = Path::new("/home/user/.local/bin");
    assert_eq!(
        update_install_dir(
            BuildKind::ReleaseSource,
            installed_source_executable,
            Some(installed_source_dir),
        )
        .unwrap(),
        installed_source_dir
    );

    let source_executable = Path::new("/opt/bettercodex/target/release/bcodex");
    assert_eq!(
        update_install_dir(
            BuildKind::ReleaseSource,
            source_executable,
            Some(Path::new("/srv/custom bin")),
        )
        .unwrap(),
        PathBuf::from("/srv/custom bin")
    );
    for cargo_destination in [
        "/opt/bettercodex/target/release",
        "/opt/bettercodex/target/debug",
        "/srv/another-checkout/target/release",
    ] {
        assert!(
            update_install_dir(
                BuildKind::ReleaseSource,
                source_executable,
                Some(Path::new(cargo_destination)),
            )
            .is_err(),
            "accepted Cargo destination {cargo_destination}"
        );
    }
    assert!(
        update_install_dir(
            BuildKind::ReleaseSource,
            Path::new("/opt/custom-cargo-output/release/bcodex"),
            Some(Path::new("/opt/custom-cargo-output/release-channel")),
        )
        .is_err()
    );
    assert_eq!(
        update_install_dir(
            BuildKind::ReleaseSource,
            source_executable,
            Some(Path::new("/home/target/user/.local/bin")),
        )
        .unwrap(),
        PathBuf::from("/home/target/user/.local/bin")
    );

    let temporary = TestDirectory::new();
    let custom_target = temporary.path().join("custom-cargo-output");
    fs::create_dir(&custom_target).unwrap();
    fs::write(custom_target.join(".rustc_info.json"), b"{}").unwrap();
    assert!(
        update_install_dir(
            BuildKind::ReleaseSource,
            source_executable,
            Some(&custom_target.join("release-channel")),
        )
        .is_err()
    );

    for invalid_destination in ["relative", "/srv/install/../target"] {
        assert!(
            update_install_dir(
                BuildKind::ReleaseSource,
                source_executable,
                Some(Path::new(invalid_destination)),
            )
            .is_err()
        );
    }
    assert!(update_install_dir(BuildKind::DebugSource, source_executable, None).is_err());
}

#[test]
fn updater_selects_the_native_installer_at_the_release_revision() {
    let revision = "2".repeat(40);
    assert_eq!(
        installer_url("owner/project", &revision),
        format!(
            "https://raw.githubusercontent.com/owner/project/{revision}/{}",
            installer_path()
        )
    );
    assert_eq!(installer_path(), "scripts/install.sh");
}

#[test]
fn updater_passes_only_the_pinned_release_install_contract() {
    let release = release_fixture("1.3.0", '2');
    let script = installer_script(&format!(
        "test \"$BCODEX_INSTALL_DIR\" = '/tmp/custom bettercodex'\n\
         test \"$BCODEX_REPOSITORY\" = owner/project\n\
         test \"$BCODEX_INSTALL_RELEASE_TAG\" = {}\n\
         test \"$BCODEX_INSTALL_ASSET_SHA256\" = {}\n\
         test \"$BCODEX_INSTALL_ASSET_SIZE\" = {}\n",
        release.tag, release.asset.sha256, release.asset.size
    ));
    run_installer_script(
        &script,
        Path::new("/tmp/custom bettercodex"),
        "owner/project",
        &release,
    )
    .unwrap();

    let error = run_installer_script(
        &installer_script("exit 7\n"),
        Path::new("/tmp/custom bettercodex"),
        "owner/project",
        &release,
    )
    .unwrap_err();
    assert!(error.to_string().contains("exit status: 7"));

    let mut early_exit = installer_script("exit 7\n");
    early_exit.resize(MAX_INSTALLER_BYTES, b'\n');
    let error = run_installer_script(
        &early_exit,
        Path::new("/tmp/custom bettercodex"),
        "owner/project",
        &release,
    )
    .unwrap_err();
    assert!(error.to_string().contains("exit status: 7"));

    assert!(
        run_installer_script(
            b"#!/bin/sh\nexit 0\n",
            Path::new("/tmp/custom bettercodex"),
            "owner/project",
            &release,
        )
        .is_err()
    );
    assert!(
        run_installer_script(
            &installer_script("exit 0\n"),
            Path::new("relative"),
            "owner/project",
            &release,
        )
        .is_err()
    );
    assert!(
        run_installer_script(
            &installer_script("exit 0\n"),
            Path::new("/tmp/custom bettercodex"),
            "invalid repository",
            &release,
        )
        .is_err()
    );

    let mut inconsistent = release.clone();
    inconsistent.version = "9.9.9".to_string();
    assert!(
        run_installer_script(
            &installer_script("exit 0\n"),
            Path::new("/tmp/custom bettercodex"),
            "owner/project",
            &inconsistent,
        )
        .is_err()
    );
    let mut invalid_asset = release;
    invalid_asset.asset.sha256 = "invalid".to_string();
    assert!(
        run_installer_script(
            &installer_script("exit 0\n"),
            Path::new("/tmp/custom bettercodex"),
            "owner/project",
            &invalid_asset,
        )
        .is_err()
    );
}

#[test]
fn update_notice_command_is_self_contained_shell_safe_and_executes_the_current_binary() {
    let quoted_path = Path::new("/tmp/bettercodex path/'quoted'/bcodex");
    let quoted_install_dir = Path::new("/tmp/bettercodex install");
    assert_eq!(
        shell_update_command(quoted_path, quoted_install_dir, "owner/project"),
        format!(
            "BCODEX_REPOSITORY=owner/project BCODEX_INSTALL_DIR={} {} update",
            shlex::try_quote(quoted_install_dir.to_str().unwrap()).unwrap(),
            shlex::try_quote(quoted_path.to_str().unwrap()).unwrap()
        )
    );
    assert!(
        shell_update_command(
            Path::new("/tmp/β/bcodex"),
            Path::new("/tmp/β/install"),
            "owner/project",
        )
        .is_ascii()
    );

    let temporary = TestDirectory::new();
    let mut executable = temporary.path().to_path_buf();
    executable.push(OsString::from_vec(
        b"bcodex space-'quote-\xff-newline-\n".to_vec(),
    ));
    let mut install_dir = temporary.path().to_path_buf();
    install_dir.push(OsString::from_vec(b"install-\xfe-newline-\n".to_vec()));
    let marker = temporary.path().join("marker");
    fs::write(
        &executable,
        b"#!/bin/sh\n[ \"$#\" -eq 1 ] && [ \"$1\" = update ] || exit 64\n[ \"$BCODEX_REPOSITORY\" = owner/project ] || exit 65\n[ \"$BCODEX_INSTALL_DIR\" = \"$BCODEX_TEST_INSTALL_DIR\" ] || exit 66\nprintf '%s' \"$1\" >\"$BCODEX_TEST_MARKER\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();

    let command = shell_update_command(&executable, &install_dir, "owner/project");
    assert!(command.is_ascii());
    assert!(!command.contains('\n'));
    let status = ProcessCommand::new("/bin/sh")
        .arg("-c")
        .arg(&command)
        .env(REPOSITORY_ENV, "wrong/repository")
        .env(INSTALL_DIR_ENV, "/wrong/install")
        .env("BCODEX_TEST_INSTALL_DIR", &install_dir)
        .env("BCODEX_TEST_MARKER", &marker)
        .status()
        .unwrap();
    assert!(status.success(), "command failed: {command}");
    assert_eq!(fs::read_to_string(marker).unwrap(), "update");
}
