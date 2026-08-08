#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import textwrap
import unittest


INSTALL_SCRIPT = Path(__file__).with_name("install.sh")
CARGO_MANIFEST = INSTALL_SCRIPT.parent.parent / "Cargo.toml"
VERSION = next(
    line.removeprefix('version = "').removesuffix('"')
    for line in CARGO_MANIFEST.read_text(encoding="utf-8").splitlines()
    if line.startswith('version = "')
)


class InstallScriptTest(unittest.TestCase):
    def test_installs_latest_linux_release_and_configures_path_once(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = run_installer(root)
            second = run_installer(root)

            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertIn("Installed bcodex", first.stdout)
            self.assertIn("Updated bcodex", second.stdout)
            self.assertIn("Restart bettercodex", second.stdout)
            self.assertNotIn("bcodex login", second.stdout)
            installed = root / "install" / "bin" / "bcodex"
            self.assertEqual(
                subprocess.run(
                    [installed, "--version"],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout,
                f"bcodex {VERSION}\n",
            )
            profile = (root / "home" / ".profile").read_text(encoding="utf-8")
            self.assertEqual(profile.count("# >>> bettercodex installer >>>"), 1)
            requests = (root / "gh.log").read_text(encoding="utf-8")
            self.assertIn("release view --repo ummay0432/bettercodex", requests)
            self.assertIn(
                "--pattern bcodex-x86_64-unknown-linux-gnu.tar.gz", requests
            )

    def test_selects_every_native_asset_and_accepts_bare_version(self) -> None:
        platforms = (
            ("Darwin", "arm64", "aarch64-apple-darwin"),
            ("Darwin", "x86_64", "x86_64-apple-darwin"),
            ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
            ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
        )
        for system, machine, target in platforms:
            with self.subTest(target=target), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result = run_installer(
                    root,
                    system=system,
                    machine=machine,
                    arguments=("--release", VERSION),
                )

                self.assertEqual(result.returncode, 0, result.stderr)
                requests = (root / "gh.log").read_text(encoding="utf-8")
                self.assertIn(f"release view v{VERSION}", requests)
                self.assertIn(f"--pattern bcodex-{target}.tar.gz", requests)

    def test_rejects_checksum_mismatch_without_replacing_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = root / "install" / "bin" / "bcodex"
            installed.parent.mkdir(parents=True)
            installed.write_text("existing binary\n", encoding="utf-8")

            result = run_installer(root, valid_checksum=False)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("failed SHA-256 verification", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")

    def test_reports_missing_github_authentication(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = run_installer(Path(directory), authenticated=False)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("run 'gh auth login'", result.stderr)

    def test_reports_repository_invitation_that_has_not_been_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = run_installer(Path(directory), repository_access=False)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("accept the repository invitation", result.stderr)

    def test_handles_spaces_and_apostrophes_in_install_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "friend's better codex"
            result = run_installer(root)

            self.assertEqual(result.returncode, 0, result.stderr)
            installed = root / "install" / "bin" / "bcodex"
            self.assertEqual(
                subprocess.run(
                    [installed, "--version"],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout,
                f"bcodex {VERSION}\n",
            )
            escaped = str(installed.parent).replace("'", "'\\''")
            profile = (root / "home" / ".profile").read_text(encoding="utf-8")
            self.assertIn(f"export PATH='{escaped}':\"$PATH\"", profile)

    def test_rejects_unsupported_architecture_before_download(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = run_installer(root, machine="riscv64")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("unsupported architecture: riscv64", result.stderr)
            requests = (root / "gh.log").read_text(encoding="utf-8")
            self.assertNotIn("release download", requests)


def run_installer(
    root: Path,
    *,
    system: str = "Linux",
    machine: str = "x86_64",
    arguments: tuple[str, ...] = (),
    authenticated: bool = True,
    repository_access: bool = True,
    valid_checksum: bool = True,
) -> subprocess.CompletedProcess[str]:
    root.mkdir(exist_ok=True)
    fake_bin = root / "fake-bin"
    fake_bin.mkdir(exist_ok=True)
    home = root / "home"
    home.mkdir(exist_ok=True)
    install_dir = root / "install" / "bin"
    archive, manifest = create_release_assets(
        root, system=system, machine=machine, valid_checksum=valid_checksum
    )

    write_executable(
        fake_bin / "uname",
        textwrap.dedent(
            f"""\
            #!/bin/sh
            case "$1" in
              -s) printf '%s\\n' '{system}' ;;
              -m) printf '%s\\n' '{machine}' ;;
            esac
            """
        ),
    )
    write_executable(
        fake_bin / "gh",
        textwrap.dedent(
            """\
            #!/bin/sh
            printf '%s\\n' "$*" >>"$BCODEX_TEST_GH_LOG"
            case "$1:$2" in
              auth:status)
                [ "$BCODEX_TEST_AUTHENTICATED" = "1" ]
                ;;
              api:repos/*)
                [ "$BCODEX_TEST_REPOSITORY_ACCESS" = "1" ]
                ;;
              release:view)
                printf 'v%s\\n' "$BCODEX_TEST_VERSION"
                ;;
              release:download)
                destination=""
                previous=""
                for argument in "$@"; do
                  if [ "$previous" = "--dir" ]; then
                    destination="$argument"
                  fi
                  previous="$argument"
                done
                [ -n "$destination" ] || exit 2
                cp "$BCODEX_TEST_ARCHIVE" "$destination/$(basename "$BCODEX_TEST_ARCHIVE")"
                cp "$BCODEX_TEST_MANIFEST" "$destination/SHA256SUMS"
                ;;
              *)
                exit 2
                ;;
            esac
            """
        ),
    )

    environment = os.environ.copy()
    for name in ("BCODEX_RELEASE", "BCODEX_REPOSITORY"):
        environment.pop(name, None)
    environment.update(
        {
            "BCODEX_INSTALL_DIR": str(install_dir),
            "BCODEX_TEST_ARCHIVE": str(archive),
            "BCODEX_TEST_AUTHENTICATED": "1" if authenticated else "0",
            "BCODEX_TEST_GH_LOG": str(root / "gh.log"),
            "BCODEX_TEST_MANIFEST": str(manifest),
            "BCODEX_TEST_REPOSITORY_ACCESS": "1" if repository_access else "0",
            "BCODEX_TEST_VERSION": VERSION,
            "HOME": str(home),
            "PATH": f"{fake_bin}:/usr/bin:/bin",
            "SHELL": "/bin/sh",
        }
    )
    return subprocess.run(
        ["/bin/sh", INSTALL_SCRIPT, *arguments],
        check=False,
        capture_output=True,
        env=environment,
        text=True,
    )


def create_release_assets(
    root: Path, *, system: str, machine: str, valid_checksum: bool
) -> tuple[Path, Path]:
    if system == "Darwin":
        architecture = "aarch64" if machine in {"arm64", "aarch64"} else "x86_64"
        target = f"{architecture}-apple-darwin"
    else:
        architecture = "aarch64" if machine in {"arm64", "aarch64"} else "x86_64"
        target = f"{architecture}-unknown-linux-gnu"

    archive = root / f"bcodex-{target}.tar.gz"
    executable = f"#!/bin/sh\nprintf 'bcodex {VERSION}\\n'\n".encode()
    with tarfile.open(archive, "w:gz") as package:
        entry = tarfile.TarInfo("bcodex")
        entry.mode = 0o755
        entry.size = len(executable)
        package.addfile(entry, io.BytesIO(executable))

    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if not valid_checksum:
        digest = "0" * 64
    manifest = root / "fixture-SHA256SUMS"
    manifest.write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    return archive, manifest


def write_executable(path: Path, contents: str) -> None:
    path.write_text(contents, encoding="utf-8")
    path.chmod(0o755)


if __name__ == "__main__":
    unittest.main()
