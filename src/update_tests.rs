use super::*;

fn release_tag_fixture(version: &str, revision: char) -> String {
    format!("bcodex-v{version}-{}", revision.to_string().repeat(40))
}

fn release_response(version: &str, revision: char) -> String {
    format!(
        "{{\"tag_name\":\"{}\",\"draft\":false,\"prerelease\":false,\"assets\":[{{\"name\":\"{RELEASE_ASSET_NAME}\",\"state\":\"uploaded\",\"size\":1024,\"digest\":\"sha256:{}\"}}]}}",
        release_tag_fixture(version, revision),
        "a".repeat(64)
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
fn accepts_only_strict_release_tags_versions_digests_and_repository_names() {
    let parsed = parse_release_tag(&release_tag_fixture("1.2.3", 'a')).unwrap();
    assert_eq!(parsed.version, "1.2.3");
    assert_eq!(parsed.revision, "a".repeat(40));

    for tag in [
        "v1.2.3-1111111111111111111111111111111111111111",
        "bcodex-v1.2-1111111111111111111111111111111111111111",
        "bcodex-v1.2.3-not-a-revision",
        "bcodex-v1.2.3-beta-1111111111111111111111111111111111111111",
        "bcodex-v1.2.3-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert!(parse_release_tag(tag).is_err(), "accepted {tag}");
    }

    assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
    assert_eq!(parse_version("1.2.3.4"), None);
    assert_eq!(parse_version("1.2.3-beta"), None);
    assert_eq!(is_newer("1.3.0", "1.2.9"), Some(true));
    assert_eq!(is_newer("1.2.9", "1.3.0"), Some(false));
    assert_eq!(is_newer("invalid", "1.3.0"), None);

    assert!(is_sha256_digest(&format!("sha256:{}", "a".repeat(64))));
    assert!(!is_sha256_digest(&format!("sha256:{}", "a".repeat(63))));
    assert!(!is_sha256_digest(&format!("sha512:{}", "a".repeat(64))));

    assert!(validate_repository("owner/public.repo").is_ok());
    assert!(validate_repository("owner").is_err());
    assert!(validate_repository("owner/repo/extra").is_err());
    assert!(validate_repository("owner/repo?ref=other").is_err());
    assert_eq!(release_tag(), None);
    assert_eq!(source_revision(), None);
}

#[test]
fn latest_release_requires_one_complete_native_asset() {
    let release = parse_latest_release(release_response("1.2.3", '1').as_bytes()).unwrap();
    assert_eq!(release.version, "1.2.3");
    assert_eq!(release.revision, "1".repeat(40));

    for response in [
        "{}".to_string(),
        release_response("1.2", '1'),
        release_response("1.2.3", '1').replace("\"draft\":false", "\"draft\":true"),
        release_response("1.2.3", '1').replace("\"prerelease\":false", "\"prerelease\":true"),
        release_response("1.2.3", '1').replace(RELEASE_ASSET_NAME, "wrong-asset.gz"),
        release_response("1.2.3", '1').replace("\"uploaded\"", "\"new\""),
        release_response("1.2.3", '1').replace("\"size\":1024", "\"size\":0"),
        release_response("1.2.3", '1').replace(&"a".repeat(64), "invalid"),
    ] {
        assert!(parse_latest_release(response.as_bytes()).is_err());
    }

    let one_asset = release_response("1.2.3", '1');
    let duplicate = one_asset.replace(
        "]}",
        &format!(
            ",{{\"name\":\"{RELEASE_ASSET_NAME}\",\"state\":\"uploaded\",\"size\":1024,\"digest\":\"sha256:{}\"}}]}}",
            "b".repeat(64)
        ),
    );
    assert!(parse_latest_release(duplicate.as_bytes()).is_err());
}

#[tokio::test]
async fn bounded_lookup_reports_only_newer_release_versions() {
    let current = parse_release_tag(&release_tag_fixture("1.2.3", '1')).unwrap();
    let (url, server) = serve_once(
        200,
        release_response("1.3.0", '2'),
        Duration::from_millis(0),
    );
    assert_eq!(
        check_for_release_update_at(&url, &current, Duration::from_secs(1)).await,
        Some(AvailableUpdate::new(&"1".repeat(40), &"2".repeat(40)))
    );
    server.join().unwrap();

    for version in ["1.2.3", "1.2.2"] {
        let (url, server) = serve_once(
            200,
            release_response(version, '2'),
            Duration::from_millis(0),
        );
        assert_eq!(
            check_for_release_update_at(&url, &current, Duration::from_secs(1)).await,
            None
        );
        server.join().unwrap();
    }
}

#[tokio::test]
async fn malformed_failed_oversized_and_timed_out_lookups_are_silent() {
    let current = parse_release_tag(&release_tag_fixture("1.2.3", '1')).unwrap();
    for (status, body) in [
        (500, b"failure".to_vec()),
        (200, br#"{"tag_name":"invalid"}"#.to_vec()),
        (200, vec![b'x'; MAX_RELEASE_METADATA_BYTES + 1]),
    ] {
        let (url, server) = serve_once(status, body, Duration::from_millis(0));
        assert_eq!(
            check_for_release_update_at(&url, &current, Duration::from_secs(1)).await,
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
        check_for_release_update_at(&url, &current, Duration::from_millis(10)).await,
        None
    );
    server.join().unwrap();
    assert_eq!(
        check_for_release_update_with("invalid repository", &current, Duration::from_millis(10),)
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
    let tag = release_tag_fixture("1.3.0", '2');
    run_installer_script(
        format!(
            "#!/bin/sh\ntest \"$BCODEX_INSTALL_DIR\" = '/tmp/custom bettercodex'\n\
             test \"$BCODEX_REPOSITORY\" = owner/project\n\
             test \"$BCODEX_INSTALL_RELEASE_TAG\" = {tag}\n"
        )
        .as_bytes(),
        Path::new("/tmp/custom bettercodex"),
        "owner/project",
        &tag,
    )
    .unwrap();
    assert!(
        run_installer_script(
            b"#!/bin/sh\nexit 7\n",
            Path::new("/tmp/custom bettercodex"),
            "owner/project",
            &tag,
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
            "not-a-release",
        )
        .unwrap_err()
        .to_string()
        .contains("invalid prefix")
    );
}
