#!/usr/bin/env python3

import gzip
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
CANDIDATE = textwrap.dedent(
    f"""\
    #!/bin/sh
    case "${{1:-}}" in
      --internal-release-tag) printf '%s\\n' "${{FIXTURE_TAG:-{TAG}}}" ;;
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
        self.curl_log = self.root / "curl.log"
        self.home.mkdir()
        self.bin.mkdir(parents=True)
        self.tools.mkdir()
        with gzip.open(self.asset, "wb") as archive:
            archive.write(CANDIDATE)
        executable(
            self.tools / "uname",
            """
            #!/bin/sh
            case "$1" in
              -s) printf '%s\\n' "$FAKE_UNAME_S" ;;
              -m) printf '%s\\n' "$FAKE_UNAME_M" ;;
              *) exit 2 ;;
            esac
            """,
        )
        executable(
            self.tools / "curl",
            """
            #!/bin/sh
            for argument do url="$argument"; done
            printf '%s\\n' "$url" >>"$CURL_LOG"
            [ "${FIXTURE_DOWNLOAD_FAIL:-0}" != 1 ] || exit 22
            cat "$FAKE_ASSET"
            """,
        )
        executable(
            self.tools / "codesign",
            """
            #!/bin/sh
            [ "${FIXTURE_CODESIGN_FAIL:-0}" != 1 ]
            """,
        )

    def close(self) -> None:
        self.temporary.cleanup()

    def env(self, **changes: str | None) -> dict[str, str]:
        values = os.environ.copy()
        values.update(
            HOME=str(self.home),
            BCODEX_INSTALL_DIR=str(self.bin),
            BCODEX_REPOSITORY="owner/project",
            FAKE_ASSET=str(self.asset),
            CURL_LOG=str(self.curl_log),
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

    def run(self, *args: str, check: bool = False, **changes: str | None):
        return subprocess.run(
            ["/bin/sh", str(INSTALLER), *args],
            env=self.env(**changes),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=check,
        )

    @property
    def installed(self) -> pathlib.Path:
        return self.bin / "bcodex"

    def urls(self) -> list[str]:
        return self.curl_log.read_text().splitlines() if self.curl_log.exists() else []


class InstallerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.f = Fixture()

    def tearDown(self) -> None:
        self.f.close()

    def test_linux_latest_install_and_obsolete_cache_cleanup(self) -> None:
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
            ["https://github.com/owner/project/releases/latest/download/bcodex-x86_64-unknown-linux-gnu.gz"],
        )
        self.assertTrue(self.f.installed.is_file())
        for name in ("build", "cargo", "rustup", "tmp", "downloads"):
            self.assertFalse((cache / name).exists())
        self.assertFalse(cache.exists())
        self.assertFalse((self.f.bin / "bcodex-path").exists())
        self.assertEqual(list(self.f.bin.glob(".bcodex-stage.*")), [])

    def test_existing_install_is_reported_as_update_without_login_guidance(self) -> None:
        self.f.installed.write_bytes(b"previous")

        result = self.f.run(check=True, BCODEX_INSTALL_RELEASE_TAG=TAG)

        self.assertIn("Updated bcodex 1.2.3", result.stdout)
        self.assertNotIn("Run: bcodex", result.stdout)
        self.assertNotIn("bcodex login", result.stdout)

    def test_standalone_developer_v8_cache_is_not_claimed_as_installer_state(self) -> None:
        developer_cache = self.f.home / ".cache" / "bettercodex" / "rusty-v8-development"
        developer_cache.mkdir(parents=True)
        self.f.run(check=True)
        self.assertTrue(developer_cache.is_dir())

    def test_pinned_macos_install(self) -> None:
        result = self.f.run(
            check=True,
            FAKE_UNAME_S="Darwin",
            FAKE_UNAME_M="arm64",
            BCODEX_INSTALL_RELEASE_TAG=TAG,
        )
        self.assertIn("macOS Apple silicon", result.stdout)
        self.assertEqual(
            self.f.urls(),
            [f"https://github.com/owner/project/releases/download/{TAG}/bcodex-aarch64-apple-darwin.gz"],
        )

    def test_unsupported_hosts_fail_before_download(self) -> None:
        for system, machine, message in (
            ("Darwin", "x86_64", "Apple silicon"),
            ("Linux", "aarch64", "x86-64 Linux"),
            ("FreeBSD", "x86_64", "only macOS and Linux"),
        ):
            result = self.f.run(FAKE_UNAME_S=system, FAKE_UNAME_M=machine)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr)
        self.assertEqual(self.f.urls(), [])

    def test_invalid_inputs_fail_before_download(self) -> None:
        for changes, message in (
            ({"BCODEX_REPOSITORY": "invalid"}, "owner/repository"),
            ({"BCODEX_INSTALL_DIR": "relative"}, "absolute path"),
            ({"BCODEX_INSTALL_RELEASE_TAG": "v1.2.3"}, "RELEASE_TAG is invalid"),
            (
                {"BCODEX_INSTALL_RELEASE_TAG": f"bcodex-v1.2.3-{'A' * 40}"},
                "RELEASE_TAG is invalid",
            ),
        ):
            result = self.f.run(**changes)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr)
        self.assertEqual(self.f.urls(), [])

    def test_verification_failures_preserve_existing_binary(self) -> None:
        self.f.installed.write_bytes(b"previous")
        cases = (
            ({"FIXTURE_TAG": f"bcodex-v9.9.9-{'9' * 40}"}, "downloaded binary is"),
            ({"FIXTURE_VERSION": "9.9.9"}, "version does not match"),
            (
                {"FIXTURE_CODESIGN_FAIL": "1", "FAKE_UNAME_S": "Darwin", "FAKE_UNAME_M": "arm64"},
                "code signature",
            ),
        )
        for changes, message in cases:
            result = self.f.run(BCODEX_INSTALL_RELEASE_TAG=TAG, **changes)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr)
            self.assertEqual(self.f.installed.read_bytes(), b"previous")
            self.assertEqual(list(self.f.bin.glob(".bcodex-stage.*")), [])

    def test_download_and_gzip_failures_preserve_existing_binary(self) -> None:
        self.f.installed.write_bytes(b"previous")
        self.assertNotEqual(self.f.run(FIXTURE_DOWNLOAD_FAIL="1").returncode, 0)
        self.assertEqual(self.f.installed.read_bytes(), b"previous")
        self.f.asset.write_bytes(b"not gzip")
        self.assertNotEqual(self.f.run().returncode, 0)
        self.assertEqual(self.f.installed.read_bytes(), b"previous")

    def test_symlink_destination_is_refused(self) -> None:
        target = self.f.root / "unrelated"
        target.write_text("keep")
        self.f.installed.symlink_to(target)
        result = self.f.run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symlinked", result.stderr)
        self.assertEqual(target.read_text(), "keep")
        self.assertEqual(self.f.urls(), [])

    def test_default_path_is_added_once(self) -> None:
        path = os.pathsep.join([str(self.f.tools), "/usr/bin", "/bin"])
        changes = {"BCODEX_INSTALL_DIR": None, "PATH": path}
        self.f.run(check=True, **changes)
        self.f.run(check=True, **changes)
        profile = self.f.home / ".profile"
        self.assertEqual(profile.read_text().count('export PATH="$HOME/.local/bin:$PATH"'), 1)
        self.assertTrue((self.f.home / ".local" / "bin" / "bcodex").is_file())

    def test_custom_install_without_home(self) -> None:
        self.f.run(check=True, HOME=None, XDG_CACHE_HOME=None)
        self.assertTrue(self.f.installed.is_file())

    def test_help_is_side_effect_free(self) -> None:
        result = self.f.run("--help", check=True)
        self.assertIn("No compilation is performed", result.stdout)
        self.assertFalse(self.f.installed.exists())
        self.assertEqual(self.f.urls(), [])


if __name__ == "__main__":
    unittest.main()
