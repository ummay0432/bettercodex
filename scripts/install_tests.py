#!/usr/bin/env python3

import gzip
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import textwrap
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
INSTALLER = ROOT / "scripts" / "install.sh"
REVISION = "1" * 40
TAG = f"bcodex-v1.2.3-{REVISION}"
LINUX_ASSET = "bcodex-x86_64-unknown-linux-gnu.gz"
MACOS_ASSET = "bcodex-aarch64-apple-darwin.gz"
CANDIDATE = textwrap.dedent(
    f"""\
    #!/bin/sh
    if [ -n "${{EXECUTION_LOG:-}}" ]; then
      printf '%s\\n' "${{1:-}}" >>"$EXECUTION_LOG"
    fi
    case "${{1:-}}" in
      --internal-release-tag) printf '%s\\n' "${{FIXTURE_TAG:-{TAG}}}" ;;
      --internal-source-revision) printf '%s\\n' "${{FIXTURE_REVISION:-{REVISION}}}" ;;
      --version) printf 'bcodex %s\\n' "${{FIXTURE_VERSION:-1.2.3}}" ;;
      *) exit 64 ;;
    esac
    """
).encode()


def executable(path: pathlib.Path, source: str) -> None:
    path.write_text(textwrap.dedent(source).lstrip())
    path.chmod(0o755)


class Fixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="bettercodex-install-tests.")
        self.root = pathlib.Path(self.temporary.name)
        assert self.root != ROOT and ROOT not in self.root.parents
        self.home = self.root / "home"
        self.bin = self.root / "install" / "bin"
        self.tools = self.root / "tools"
        self.asset = self.root / "candidate.gz"
        self.metadata = self.root / "release.json"
        self.curl_log = self.root / "curl.log"
        self.execution_log = self.root / "execution.log"
        self.home.mkdir()
        self.bin.mkdir(parents=True)
        self.tools.mkdir()
        self.write_asset(CANDIDATE)
        executable(
            self.tools / "uname",
            """
            #!/bin/sh
            case "$1" in
              -s) printf '%s\n' "$FAKE_UNAME_S" ;;
              -m) printf '%s\n' "$FAKE_UNAME_M" ;;
              *) exit 2 ;;
            esac
            """,
        )
        executable(
            self.tools / "curl",
            """
            #!/bin/sh
            if [ -f "$HOME/.curlrc" ] && [ "${1:-}" != "--disable" ]; then
              exit 97
            fi
            url=""
            output=""
            previous=""
            for argument do
              case "$argument" in
                https://*) url="$argument" ;;
              esac
              if [ "$previous" = "--output" ]; then
                output="$argument"
              fi
              previous="$argument"
            done
            printf '%s\n' "$url" >>"$CURL_LOG"
            case "$url" in
              https://api.github.com/*)
                [ "${FIXTURE_METADATA_FAIL:-0}" != 1 ] || exit 22
                cat "$FAKE_METADATA" >"$output" || exit
                if [ -n "${FIXTURE_METADATA_COMPLETE_MARKER:-}" ]; then
                  : >"$FIXTURE_METADATA_COMPLETE_MARKER"
                fi
                ;;
              https://github.com/*/releases/download/*)
                [ "${FIXTURE_DOWNLOAD_FAIL:-0}" != 1 ] || exit 22
                cp "$FAKE_ASSET" "$output"
                ;;
              *) exit 22 ;;
            esac
            """,
        )
        executable(
            self.tools / "codesign",
            """
            #!/bin/sh
            [ "${FIXTURE_CODESIGN_FAIL:-0}" != 1 ]
            """,
        )
        executable(
            self.tools / "mv",
            """
            #!/bin/sh
            [ "${FIXTURE_MV_FAIL:-0}" != 1 ] || exit 1
            if [ "${FIXTURE_MV_DESTINATION_DIRECTORY:-0}" = 1 ]; then
              destination=""
              for argument do destination="$argument"; done
              mkdir -p "$destination"
            fi
            exec /bin/mv "$@"
            """,
        )
        executable(
            self.tools / "rm",
            """
            #!/bin/sh
            if [ "${FIXTURE_RM_TRANSACTION_FAIL:-0}" = 1 ]; then
              for argument do
                case "$argument" in
                  */.bcodex-transaction.*)
                    [ ! -d "$argument" ] || exit 1
                    ;;
                esac
              done
            fi
            exec /bin/rm "$@"
            """,
        )
        executable(
            self.tools / "sleep",
            """
            #!/bin/sh
            exit 0
            """,
        )

    def close(self) -> None:
        self.temporary.cleanup()

    def write_asset(self, content: bytes) -> None:
        with gzip.open(self.asset, "wb") as archive:
            archive.write(content)

    @property
    def asset_bytes(self) -> bytes:
        return self.asset.read_bytes()

    @property
    def asset_sha256(self) -> str:
        return hashlib.sha256(self.asset_bytes).hexdigest()

    @property
    def asset_size(self) -> int:
        return len(self.asset_bytes)

    def release_metadata(self, **changes: object) -> str:
        digest = f"sha256:{self.asset_sha256}"
        size = self.asset_size
        document: dict[str, object] = {
            "tag_name": TAG,
            "target_commitish": REVISION,
            "draft": False,
            "prerelease": False,
            "immutable": True,
            "assets": [
                {
                    "name": LINUX_ASSET,
                    "state": "uploaded",
                    "size": size,
                    "digest": digest,
                },
                {
                    "name": MACOS_ASSET,
                    "state": "uploaded",
                    "size": size,
                    "digest": digest,
                },
            ],
        }
        document.update(changes)
        return json.dumps(document, separators=(",", ":"))

    def env(self, **changes: str | None) -> dict[str, str]:
        values = os.environ.copy()
        for name in (
            "BCODEX_INSTALL_ASSET_SHA256",
            "BCODEX_INSTALL_ASSET_SIZE",
            "BCODEX_INSTALL_RELEASE_TAG",
            "FIXTURE_METADATA_COMPLETE_MARKER",
            "FIXTURE_MV_DESTINATION_DIRECTORY",
            "FIXTURE_RM_TRANSACTION_FAIL",
            "XDG_CACHE_HOME",
        ):
            values.pop(name, None)
        values.update(
            HOME=str(self.home),
            BCODEX_INSTALL_DIR=str(self.bin),
            BCODEX_REPOSITORY="owner/project",
            FAKE_ASSET=str(self.asset),
            FAKE_METADATA=str(self.metadata),
            CURL_LOG=str(self.curl_log),
            EXECUTION_LOG=str(self.execution_log),
            FAKE_UNAME_S="Linux",
            FAKE_UNAME_M="x86_64",
            SHELL="/bin/sh",
            PATH=os.pathsep.join([str(self.tools), str(self.bin), "/usr/bin", "/bin"]),
        )
        for name, value in changes.items():
            if value is None:
                values.pop(name, None)
            else:
                values[name] = value
        return values

    def run(
        self,
        *args: str,
        check: bool = False,
        metadata: str | None = None,
        **changes: str | None,
    ) -> subprocess.CompletedProcess[str]:
        self.metadata.write_text(metadata if metadata is not None else self.release_metadata())
        return subprocess.run(
            ["/bin/sh", str(INSTALLER), *args],
            env=self.env(**changes),
            text=True,
            errors="surrogateescape",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=check,
            timeout=20,
        )

    @property
    def installed(self) -> pathlib.Path:
        return self.bin / "bcodex"

    def urls(self) -> list[str]:
        return self.curl_log.read_text().splitlines() if self.curl_log.exists() else []

    def executions(self) -> list[str]:
        return (
            self.execution_log.read_text().splitlines()
            if self.execution_log.exists()
            else []
        )

    def reset_logs(self) -> None:
        self.curl_log.unlink(missing_ok=True)
        self.execution_log.unlink(missing_ok=True)

    def assert_no_transactions(self, test: unittest.TestCase) -> None:
        test.assertEqual(list(self.bin.glob(".bcodex-transaction.*")), [])
        test.assertEqual(list(self.bin.glob(".bcodex-stage.*")), [])
        test.assertFalse((self.bin / ".bcodex-install.lock").exists())


class InstallerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.f = Fixture()

    def tearDown(self) -> None:
        self.f.close()

    def test_linux_latest_install_verifies_metadata_and_cleans_owned_state(self) -> None:
        cache = self.f.home / ".cache" / "bettercodex"
        for name in ("build", "cargo", "rustup", "tmp", "downloads"):
            (cache / name).mkdir(parents=True)
        (cache / "rusty-v8-obsolete").mkdir()
        (self.f.bin / "bcodex-path").mkdir()

        result = self.f.run(check=True)

        self.assertIn("Installed bcodex 1.2.3", result.stdout)
        self.assertIn("Run: bcodex", result.stdout)
        self.assertNotIn("bcodex login", result.stdout)
        self.assertEqual(
            self.f.urls(),
            [
                "https://api.github.com/repos/owner/project/releases/latest",
                f"https://github.com/owner/project/releases/download/{TAG}/{LINUX_ASSET}",
            ],
        )
        self.assertEqual(
            self.f.executions(),
            ["--internal-release-tag", "--internal-source-revision", "--version"],
        )
        self.assertEqual(self.f.installed.read_bytes(), CANDIDATE)
        for name in ("build", "cargo", "rustup", "tmp", "downloads"):
            self.assertFalse((cache / name).exists())
        self.assertFalse(cache.exists())
        self.assertFalse((self.f.bin / "bcodex-path").exists())
        self.f.assert_no_transactions(self)

    def test_updater_attestation_avoids_a_duplicate_metadata_request(self) -> None:
        self.f.installed.write_bytes(b"previous")

        result = self.f.run(
            check=True,
            BCODEX_INSTALL_RELEASE_TAG=TAG,
            BCODEX_INSTALL_ASSET_SHA256=self.f.asset_sha256,
            BCODEX_INSTALL_ASSET_SIZE=str(self.f.asset_size),
        )

        self.assertIn("Updated bcodex 1.2.3", result.stdout)
        self.assertNotIn("Run: bcodex", result.stdout)
        self.assertEqual(
            self.f.urls(),
            [f"https://github.com/owner/project/releases/download/{TAG}/{LINUX_ASSET}"],
        )
        self.f.assert_no_transactions(self)

    def test_pinned_macos_install_validates_the_exact_release(self) -> None:
        result = self.f.run(
            check=True,
            FAKE_UNAME_S="Darwin",
            FAKE_UNAME_M="arm64",
            BCODEX_INSTALL_RELEASE_TAG=TAG,
        )

        self.assertIn("macOS Apple silicon", result.stdout)
        self.assertEqual(
            self.f.urls(),
            [
                f"https://api.github.com/repos/owner/project/releases/tags/{TAG}",
                f"https://github.com/owner/project/releases/download/{TAG}/{MACOS_ASSET}",
            ],
        )

    def test_unsupported_hosts_fail_before_network_or_installation(self) -> None:
        for system, machine, message in (
            ("Darwin", "x86_64", "Apple silicon"),
            ("Linux", "aarch64", "x86-64 Linux"),
            ("FreeBSD", "x86_64", "only macOS and Linux"),
        ):
            result = self.f.run(FAKE_UNAME_S=system, FAKE_UNAME_M=machine)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr)
        self.assertEqual(self.f.urls(), [])

    def test_invalid_overrides_fail_before_network(self) -> None:
        cases = (
            ({"BCODEX_REPOSITORY": "invalid"}, "owner/repository"),
            ({"BCODEX_REPOSITORY": "../project"}, "owner/repository"),
            ({"BCODEX_REPOSITORY": "owner/.."}, "owner/repository"),
            ({"BCODEX_REPOSITORY": os.fsdecode(b"owner/proj\xffct")}, "owner/repository"),
            ({"BCODEX_INSTALL_DIR": "relative"}, "absolute path"),
            (
                {"BCODEX_INSTALL_DIR": str(self.f.root / "install" / ".." / "target")},
                "parent-directory components",
            ),
            ({"BCODEX_INSTALL_DIR": None, "HOME": "relative"}, "HOME must be an absolute path"),
            ({"BCODEX_INSTALL_RELEASE_TAG": "v1.2.3"}, "RELEASE_TAG is invalid"),
            (
                {"BCODEX_INSTALL_RELEASE_TAG": f"bcodex-v1.2.3-{'A' * 40}"},
                "RELEASE_TAG is invalid",
            ),
            (
                {"BCODEX_INSTALL_RELEASE_TAG": f"bcodex-v01.2.3-{REVISION}"},
                "RELEASE_TAG is invalid",
            ),
            (
                {
                    "BCODEX_INSTALL_RELEASE_TAG":
                        f"bcodex-v18446744073709551616.0.0-{REVISION}"
                },
                "RELEASE_TAG is invalid",
            ),
            ({"BCODEX_INSTALL_ASSET_SHA256": "a" * 64}, "require BCODEX_INSTALL_RELEASE_TAG"),
            (
                {
                    "BCODEX_INSTALL_RELEASE_TAG": TAG,
                    "BCODEX_INSTALL_ASSET_SHA256": "a" * 64,
                },
                "must be set together",
            ),
            (
                {
                    "BCODEX_INSTALL_RELEASE_TAG": TAG,
                    "BCODEX_INSTALL_ASSET_SHA256": "invalid",
                    "BCODEX_INSTALL_ASSET_SIZE": "1",
                },
                "ASSET_SHA256 is invalid",
            ),
            (
                {
                    "BCODEX_INSTALL_RELEASE_TAG": TAG,
                    "BCODEX_INSTALL_ASSET_SHA256": "a" * 64,
                    "BCODEX_INSTALL_ASSET_SIZE": "0",
                },
                "ASSET_SIZE is invalid",
            ),
            (
                {
                    "BCODEX_INSTALL_RELEASE_TAG": TAG,
                    "BCODEX_INSTALL_ASSET_SHA256": "a" * 64,
                    "BCODEX_INSTALL_ASSET_SIZE": "134217729",
                },
                "ASSET_SIZE is invalid",
            ),
        )
        for changes, message in cases:
            with self.subTest(changes=changes):
                result = self.f.run(**changes)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
        self.assertEqual(self.f.urls(), [])

    def test_metadata_parser_ignores_nested_decoys_and_field_order(self) -> None:
        base = json.loads(self.f.release_metadata())
        asset = dict(base["assets"][0])
        asset["nested"] = {
            "name": "wrong.gz",
            "state": "new",
            "size": 0,
            "digest": "sha256:invalid",
        }
        reordered = {
            "assets": [asset],
            "decoy": {
                "tag_name": f"bcodex-v9.9.9-{'9' * 40}",
                "target_commitish": "9" * 40,
                "draft": True,
                "prerelease": True,
                "immutable": False,
            },
            "immutable": True,
            "prerelease": False,
            "draft": False,
            "target_commitish": REVISION,
            "tag_name": TAG,
        }

        self.f.run(check=True, metadata=json.dumps(reordered))

    def test_cargo_artifact_destinations_are_refused_before_network(self) -> None:
        named_target = self.f.root / "checkout" / "target" / "release"
        custom_target = self.f.root / "custom-cargo-output"
        custom_target.mkdir()
        (custom_target / ".rustc_info.json").write_text("{}")
        custom_destination = custom_target / "release-channel"

        profile_examples = (
            self.f.root / "aliased-checkout" / "target" / "release" / "examples"
        )
        profile_examples.mkdir(parents=True)
        profile_alias = self.f.root / "cargo-profile-alias"
        profile_alias.symlink_to(profile_examples, target_is_directory=True)
        aliased_destination = profile_alias / "release-channel"

        for destination in (named_target, custom_destination, aliased_destination):
            with self.subTest(destination=destination):
                self.f.reset_logs()
                result = self.f.run(BCODEX_INSTALL_DIR=str(destination))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Cargo artifact path", result.stderr)
                self.assertEqual(self.f.urls(), [])
                self.assertFalse(destination.exists())

    def test_release_metadata_must_be_full_immutable_and_self_consistent(self) -> None:
        base = json.loads(self.f.release_metadata())
        cases: list[tuple[str, str, str]] = []
        for field, value, message in (
            ("draft", True, "draft or prerelease"),
            ("prerelease", True, "draft or prerelease"),
            ("immutable", False, "not immutable"),
            ("target_commitish", "2" * 40, "target does not match"),
            ("tag_name", f"bcodex-v1.2.3-beta-{REVISION}", "tag is invalid"),
        ):
            changed = dict(base)
            changed[field] = value
            cases.append((field, json.dumps(changed), message))

        target = base["assets"][0]
        asset_cases = (
            ([], f"no unique {LINUX_ASSET}"),
            ([target, dict(target)], f"no unique {LINUX_ASSET}"),
            ([{**target, "name": "wrong.gz"}], f"no unique {LINUX_ASSET}"),
            ([{**target, "state": "new"}], "not uploaded"),
            ([{**target, "size": 0}], "size is invalid"),
            ([{**target, "size": 134217729}], "size is invalid"),
            ([{**target, "digest": "sha256:invalid"}], "digest is invalid"),
        )
        for assets, message in asset_cases:
            changed = dict(base)
            changed["assets"] = assets
            cases.append((message, json.dumps(changed), message))
        duplicate_tag = self.f.release_metadata().replace(
            '"tag_name":', f'"tag_name":"{TAG}","tag_name":', 1
        )
        duplicate_assets = self.f.release_metadata().replace(
            '"assets":', '"assets":[],"assets":', 1
        )
        cases.append(("duplicate tag", duplicate_tag, "no unique tag"))
        cases.append(("duplicate assets", duplicate_assets, "malformed"))
        cases.append(("root array", f"[{self.f.release_metadata()}]", "malformed"))
        cases.append(("concatenated", self.f.release_metadata() + "{}", "malformed"))
        cases.append(("malformed", '{"tag_name":', "malformed"))

        self.f.installed.write_bytes(b"previous")
        for name, metadata, message in cases:
            with self.subTest(case=name):
                self.f.reset_logs()
                result = self.f.run(metadata=metadata)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertEqual(self.f.installed.read_bytes(), b"previous")
                self.assertEqual(self.f.executions(), [])
                self.assertEqual(
                    self.f.urls(),
                    ["https://api.github.com/repos/owner/project/releases/latest"],
                )
                self.f.assert_no_transactions(self)

    def test_metadata_http_and_size_failures_are_bounded(self) -> None:
        result = self.f.run(FIXTURE_METADATA_FAIL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not fetch bounded", result.stderr)
        self.assertEqual(self.f.executions(), [])
        self.f.assert_no_transactions(self)

        self.f.reset_logs()
        transfer_complete = self.f.root / "metadata-transfer-complete"
        result = self.f.run(
            metadata="x" * (1048576 + 1),
            FIXTURE_METADATA_COMPLETE_MARKER=str(transfer_complete),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not fetch bounded", result.stderr)
        self.assertFalse(transfer_complete.exists())
        self.assertEqual(self.f.executions(), [])
        self.f.assert_no_transactions(self)

    def test_unverified_archives_are_never_executed_or_installed(self) -> None:
        self.f.installed.write_bytes(b"previous")
        base = json.loads(self.f.release_metadata())
        target = base["assets"][0]

        wrong_digest = dict(base)
        wrong_digest["assets"] = [{**target, "digest": f"sha256:{'0' * 64}"}]
        wrong_size = dict(base)
        wrong_size["assets"] = [{**target, "size": self.f.asset_size + 1}]

        for name, metadata, message in (
            ("digest", json.dumps(wrong_digest), "SHA-256 does not match"),
            ("size", json.dumps(wrong_size), "size does not match"),
        ):
            with self.subTest(case=name):
                self.f.reset_logs()
                result = self.f.run(metadata=metadata)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertEqual(self.f.executions(), [])
                self.assertEqual(self.f.installed.read_bytes(), b"previous")
                self.f.assert_no_transactions(self)

        self.f.reset_logs()
        self.f.asset.write_bytes(b"not gzip")
        result = self.f.run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("valid gzip", result.stderr)
        self.assertEqual(self.f.executions(), [])
        self.assertEqual(self.f.installed.read_bytes(), b"previous")
        self.f.assert_no_transactions(self)

    def test_macos_signature_is_checked_before_binary_execution(self) -> None:
        self.f.installed.write_bytes(b"previous")

        result = self.f.run(
            FAKE_UNAME_S="Darwin",
            FAKE_UNAME_M="arm64",
            FIXTURE_CODESIGN_FAIL="1",
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("code signature", result.stderr)
        self.assertEqual(self.f.executions(), [])
        self.assertEqual(self.f.installed.read_bytes(), b"previous")
        self.f.assert_no_transactions(self)

    def test_verified_binary_identity_failures_preserve_existing_install(self) -> None:
        self.f.installed.write_bytes(b"previous")
        cases = (
            ({"FIXTURE_TAG": f"bcodex-v9.9.9-{'9' * 40}"}, "downloaded binary is"),
            ({"FIXTURE_REVISION": "9" * 40}, "source revision does not match"),
            ({"FIXTURE_VERSION": "9.9.9"}, "version does not match"),
        )
        for changes, message in cases:
            with self.subTest(changes=changes):
                self.f.reset_logs()
                result = self.f.run(**changes)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertEqual(self.f.installed.read_bytes(), b"previous")
                self.f.assert_no_transactions(self)

    def test_atomic_replace_failure_does_not_report_success_or_damage_existing_binary(self) -> None:
        self.f.installed.write_bytes(b"previous")

        result = self.f.run(FIXTURE_MV_FAIL="1")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not atomically replace", result.stderr)
        self.assertNotIn("Updated bcodex", result.stdout)
        self.assertEqual(self.f.installed.read_bytes(), b"previous")
        self.f.assert_no_transactions(self)

    def test_destination_directory_race_is_not_reported_as_success(self) -> None:
        result = self.f.run(FIXTURE_MV_DESTINATION_DIRECTORY="1")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("destination changed during atomic replacement", result.stderr)
        self.assertNotIn("Installed bcodex", result.stdout)
        self.assertTrue(self.f.installed.is_dir())
        self.assertEqual(list(self.f.installed.iterdir()), [])
        self.f.assert_no_transactions(self)

    def test_download_failure_preserves_existing_binary(self) -> None:
        self.f.installed.write_bytes(b"previous")
        result = self.f.run(FIXTURE_DOWNLOAD_FAIL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.f.installed.read_bytes(), b"previous")
        self.assertEqual(self.f.executions(), [])
        self.f.assert_no_transactions(self)

    def test_failed_cleanup_retains_a_recoverable_install_lock(self) -> None:
        self.f.installed.write_bytes(b"previous")

        result = self.f.run(
            FIXTURE_DOWNLOAD_FAIL="1",
            FIXTURE_RM_TRANSACTION_FAIL="1",
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("retaining installer lock", result.stderr)
        self.assertEqual(self.f.installed.read_bytes(), b"previous")
        self.assertTrue((self.f.bin / ".bcodex-install.lock").is_dir())
        self.assertEqual(len(list(self.f.bin.glob(".bcodex-transaction.*"))), 1)

        self.f.run(check=True)
        self.f.assert_no_transactions(self)
        self.assertEqual(self.f.installed.read_bytes(), CANDIDATE)

    def test_symlink_destination_is_refused_before_network(self) -> None:
        target = self.f.root / "unrelated"
        target.write_text("keep")
        self.f.installed.symlink_to(target)

        result = self.f.run()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symlinked", result.stderr)
        self.assertEqual(target.read_text(), "keep")
        self.assertEqual(self.f.urls(), [])
        self.f.assert_no_transactions(self)

    def test_stale_lock_and_transaction_are_reclaimed(self) -> None:
        lock = self.f.bin / ".bcodex-install.lock"
        lock.mkdir()
        (lock / "pid").write_text("99999999\n")
        stale = self.f.bin / ".bcodex-transaction.stale"
        stale.mkdir()
        (stale / "partial").write_text("partial")

        self.f.run(check=True)

        self.f.assert_no_transactions(self)

    def test_unreclaimable_stale_lock_fails_without_spinning_or_network(self) -> None:
        lock = self.f.bin / ".bcodex-install.lock"
        lock.mkdir()
        (lock / "pid").write_text("99999999\n")

        result = self.f.run(FIXTURE_MV_FAIL="1")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not reclaim stale installer lock", result.stderr)
        self.assertEqual(self.f.urls(), [])

    def test_live_install_lock_refuses_concurrent_install_before_network(self) -> None:
        lock = self.f.bin / ".bcodex-install.lock"
        lock.mkdir()
        (lock / "pid").write_text(f"{os.getpid()}\n")

        result = self.f.run()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("another bettercodex install", result.stderr)
        self.assertEqual(self.f.urls(), [])
        (lock / "pid").unlink()
        lock.rmdir()

    def test_default_path_is_added_once(self) -> None:
        path = os.pathsep.join([str(self.f.tools), "/usr/bin", "/bin"])
        changes = {"BCODEX_INSTALL_DIR": None, "PATH": path}
        self.f.run(check=True, **changes)
        self.f.run(check=True, **changes)
        profile = self.f.home / ".profile"
        self.assertEqual(profile.read_text().count('export PATH="$HOME/.local/bin:$PATH"'), 1)
        self.assertTrue((self.f.home / ".local" / "bin" / "bcodex").is_file())

    def test_default_path_uses_the_platform_shell_startup_file(self) -> None:
        cases = (
            ("Linux", "/bin/bash", ".bashrc"),
            ("Linux", "/bin/zsh", ".zshrc"),
            ("Darwin", "/bin/bash", ".bash_profile"),
            ("Darwin", "/bin/zsh", ".zprofile"),
        )
        profile_names = {profile for _, _, profile in cases} | {".profile"}
        for system, shell, expected_profile in cases:
            with self.subTest(system=system, shell=shell):
                fixture = Fixture()
                try:
                    path = os.pathsep.join([str(fixture.tools), "/usr/bin", "/bin"])
                    fixture.run(
                        check=True,
                        BCODEX_INSTALL_DIR=None,
                        PATH=path,
                        SHELL=shell,
                        FAKE_UNAME_S=system,
                        FAKE_UNAME_M="arm64" if system == "Darwin" else "x86_64",
                    )
                    self.assertIn(
                        'export PATH="$HOME/.local/bin:$PATH"',
                        (fixture.home / expected_profile).read_text(),
                    )
                    self.assertEqual(
                        {
                            profile
                            for profile in profile_names
                            if (fixture.home / profile).exists()
                        },
                        {expected_profile},
                    )
                finally:
                    fixture.close()

    def test_user_curl_configuration_cannot_change_downloads(self) -> None:
        (self.f.home / ".curlrc").write_text("insecure\nurl = https://example.invalid\n")

        self.f.run(check=True)

        self.assertEqual(self.f.installed.read_bytes(), CANDIDATE)
        self.f.assert_no_transactions(self)

    def test_custom_install_without_home_supports_unusual_paths(self) -> None:
        raw_directory = os.fsencode(self.f.root) + b"/install space-'quote-\xff-\n/bin"
        os.makedirs(raw_directory)
        install_dir = os.fsdecode(raw_directory)

        result = self.f.run(
            check=True,
            HOME=None,
            XDG_CACHE_HOME=None,
            BCODEX_INSTALL_DIR=install_dir,
        )

        self.assertIn("Installed bcodex 1.2.3 at bytes:0x", result.stdout)
        self.assertIn("install directory bytes:0x", result.stderr)
        self.assertNotIn(install_dir, result.stdout + result.stderr)
        self.assertTrue(os.path.isfile(raw_directory + b"/bcodex"))

    def test_symlinked_cache_home_is_not_claimed_as_legacy_installer_state(self) -> None:
        external = self.f.root / "developer-cache"
        (external / "bettercodex" / "build").mkdir(parents=True)
        (self.f.home / ".cache").symlink_to(external, target_is_directory=True)

        self.f.run(check=True)

        self.assertTrue((external / "bettercodex" / "build").is_dir())

    def test_help_is_side_effect_free(self) -> None:
        result = self.f.run("--help", check=True)
        self.assertIn("No compilation is performed", result.stdout)
        self.assertIn("ASSET_SHA256", result.stdout)
        self.assertFalse(self.f.installed.exists())
        self.assertEqual(self.f.urls(), [])


if __name__ == "__main__":
    unittest.main()
