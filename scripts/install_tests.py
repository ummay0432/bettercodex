#!/usr/bin/env python3

from __future__ import annotations

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
COMMIT = "a" * 40
NEXT_COMMIT = "b" * 40


class InstallScriptTest(unittest.TestCase):
    def test_installs_current_main_and_reuses_the_build_cache(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = run_installer(root)
            second = run_installer(root)

            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertIn("Installed bcodex", first.stdout)
            self.assertIn("Updated bcodex", second.stdout)
            self.assertIn("Restart bettercodex", second.stdout)
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
            self.assertIn("api repos/ummay0432/bettercodex/commits/main", requests)
            self.assertIn(f"api repos/ummay0432/bettercodex/tarball/{COMMIT}", requests)
            builds = (root / "build.log").read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(builds), 2)
            self.assertTrue(
                all(
                    build.startswith(COMMIT)
                    and str(root / "build cache" / "target") in build
                    for build in builds
                )
            )
            binary_calls = (root / "binary.log").read_text(encoding="utf-8").splitlines()
            self.assertEqual(binary_calls.count("--internal-source-revision"), 2)
            self.assertEqual(binary_calls.count("--internal-package-smoke"), 2)
            self.assertEqual(binary_calls.count("--tool-context-json"), 2)

    def test_retries_when_main_advances_during_the_build(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = run_installer(root, next_commit=NEXT_COMMIT)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(
                f"Main advanced from {COMMIT[:12]} to {NEXT_COMMIT[:12]}",
                result.stdout,
            )
            requests = (root / "gh.log").read_text(encoding="utf-8")
            self.assertIn(f"tarball/{COMMIT}", requests)
            self.assertIn(f"tarball/{NEXT_COMMIT}", requests)
            builds = (root / "build.log").read_text(encoding="utf-8").splitlines()
            self.assertEqual(
                [build.split(maxsplit=1)[0] for build in builds],
                [COMMIT, NEXT_COMMIT],
            )

    def test_accepts_explicit_versions_on_every_supported_native_host(self) -> None:
        platforms = (
            ("Darwin", "arm64", "darwin aarch64"),
            ("Darwin", "x86_64", "darwin x86_64"),
            ("Linux", "aarch64", "linux aarch64"),
            ("Linux", "x86_64", "linux x86_64"),
        )
        for system, machine, label in platforms:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result = run_installer(
                    root,
                    system=system,
                    machine=machine,
                    arguments=("--release", VERSION),
                )

                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"for {label}", result.stdout)
                requests = (root / "gh.log").read_text(encoding="utf-8")
                self.assertNotIn("tags?per_page", requests)
                self.assertIn(
                    f"api repos/ummay0432/bettercodex/commits/v{VERSION}", requests
                )

    def test_failed_build_does_not_replace_an_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(root, build_success=False)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("local bettercodex compilation failed", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")

    def test_wrong_built_version_does_not_replace_an_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(root, built_version="9.9.9")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(f"did not report bcodex {VERSION}", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")

    def test_wrong_embedded_revision_does_not_replace_an_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(root, embedded_revision=NEXT_COMMIT)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(f"did not embed source revision {COMMIT}", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")

    def test_failed_runtime_smoke_does_not_replace_an_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(root, runtime_smoke_success=False)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("V8 and ICU runtime smoke test", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")

    def test_failed_resource_smoke_does_not_replace_an_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(root, resource_smoke_success=False)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("embedded-resource smoke test", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")

    def test_tag_and_manifest_version_must_match_before_building(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(
                root,
                source_version="9.9.9",
                arguments=("--release", VERSION),
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                f"source tag v{VERSION} contains package version 9.9.9", result.stderr
            )
            self.assertFalse((root / "build.log").exists())

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

    def test_handles_spaces_and_apostrophes_in_install_and_build_paths(self) -> None:
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

    def test_rejects_unsupported_architecture_before_source_download(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = run_installer(root, machine="riscv64")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("unsupported architecture: riscv64", result.stderr)
            self.assertFalse((root / "gh.log").exists())


def existing_binary(root: Path) -> Path:
    installed = root / "install" / "bin" / "bcodex"
    installed.parent.mkdir(parents=True)
    installed.write_text("existing binary\n", encoding="utf-8")
    return installed


def run_installer(
    root: Path,
    *,
    system: str = "Linux",
    machine: str = "x86_64",
    arguments: tuple[str, ...] = (),
    authenticated: bool = True,
    repository_access: bool = True,
    build_success: bool = True,
    built_version: str = VERSION,
    source_version: str = VERSION,
    runtime_smoke_success: bool = True,
    resource_smoke_success: bool = True,
    next_commit: str = COMMIT,
    embedded_revision: str | None = None,
) -> subprocess.CompletedProcess[str]:
    root.mkdir(parents=True, exist_ok=True)
    fake_bin = root / "fake-bin"
    fake_bin.mkdir(exist_ok=True)
    home = root / "home"
    home.mkdir(exist_ok=True)
    install_dir = root / "install" / "bin"
    build_dir = root / "build cache"
    archive = create_source_archive(root, source_version)

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
              api:*)
                case "$2" in
                  "repos/$BCODEX_TEST_REPOSITORY")
                    [ "$BCODEX_TEST_REPOSITORY_ACCESS" = "1" ]
                    ;;
                  "repos/$BCODEX_TEST_REPOSITORY/commits/main")
                    count=0
                    if [ -f "$BCODEX_TEST_COMMIT_COUNT_FILE" ]; then
                      count="$(sed -n '1p' "$BCODEX_TEST_COMMIT_COUNT_FILE")"
                    fi
                    count=$((count + 1))
                    printf '%s\\n' "$count" >"$BCODEX_TEST_COMMIT_COUNT_FILE"
                    if [ "$count" -eq 1 ]; then
                      printf '%s\\n' "$BCODEX_TEST_COMMIT"
                    else
                      printf '%s\\n' "$BCODEX_TEST_NEXT_COMMIT"
                    fi
                    ;;
                  repos/$BCODEX_TEST_REPOSITORY/commits/*)
                    printf '%s\\n' "$BCODEX_TEST_COMMIT"
                    ;;
                  repos/$BCODEX_TEST_REPOSITORY/tarball/*)
                    cat "$BCODEX_TEST_SOURCE_ARCHIVE"
                    ;;
                  *)
                    exit 2
                    ;;
                esac
                ;;
              *)
                exit 2
                ;;
            esac
            """
        ),
    )
    write_executable(
        fake_bin / "python3",
        textwrap.dedent(
            """\
            #!/bin/sh
            printf '%s %s\\n' "$BCODEX_SOURCE_REVISION" "$*" >>"$BCODEX_TEST_BUILD_LOG"
            [ "$BCODEX_TEST_BUILD_SUCCESS" = "1" ] || exit 9
            target=""
            while [ "$#" -gt 0 ]; do
              if [ "$1" = "--target-dir" ]; then
                shift
                target="$1"
              fi
              shift
            done
            [ -n "$target" ] || exit 10
            mkdir -p "$target/release"
            {
              printf '%s\\n' '#!/bin/sh'
              printf 'version=%s\\n' "$BCODEX_TEST_BUILT_VERSION"
              printf 'source_revision=%s\\n' "${BCODEX_TEST_EMBEDDED_REVISION:-$BCODEX_SOURCE_REVISION}"
              printf 'runtime_smoke=%s\\n' "$BCODEX_TEST_RUNTIME_SMOKE"
              printf 'resource_smoke=%s\\n' "$BCODEX_TEST_RESOURCE_SMOKE"
              printf '%s\\n' 'if [ -n "${BCODEX_TEST_BINARY_LOG:-}" ]; then'
              printf '%s\\n' '  printf "%s\\n" "$*" >>"$BCODEX_TEST_BINARY_LOG"'
              printf '%s\\n' 'fi'
              printf '%s\\n' 'case "${1:-}" in'
              printf '%s\\n' '  --version) printf "bcodex %s\\n" "$version" ;;'
              printf '%s\\n' '  --internal-source-revision) printf "%s\\n" "$source_revision" ;;'
              printf '%s\\n' '  --internal-package-smoke)'
              printf '%s\\n' '    [ "$runtime_smoke" = 1 ] || exit 11'
              printf '%s\\n' '    printf "bcodex %s package smoke passed\\n" "$version"'
              printf '%s\\n' '    ;;'
              printf '%s\\n' '  --tool-context-json)'
              printf '%s\\n' '    [ "$resource_smoke" = 1 ] || exit 12'
              printf '%s\\n' '    mkdir -p "$BCODEX_HOME/skills/.system/loop/references"'
              printf '%s\\n' '    mkdir -p "$BCODEX_HOME/skills/.system/openai-docs/scripts"'
              printf '%s\\n' '    printf "fixture\\n" >"$BCODEX_HOME/skills/.system/loop/references/evals-manifest.md"'
              printf '%s\\n' '    printf "fixture\\n" >"$BCODEX_HOME/skills/.system/openai-docs/SKILL.md"'
              printf '%s\\n' '    printf "fixture\\n" >"$BCODEX_HOME/skills/.system/openai-docs/scripts/resolve-latest-model-info.cjs"'
              printf '%s\\n' '    printf "{\\\"tool\\\":\\\"openaiDeveloperDocs__search_openai_docs\\\"}\\n"'
              printf '%s\\n' '    ;;'
              printf '%s\\n' '  *) exit 13 ;;'
              printf '%s\\n' 'esac'
            } >"$target/release/bcodex"
            chmod 0755 "$target/release/bcodex"
            """
        ),
    )
    for command in ("cargo", "cc", "rustc", "rustup"):
        write_executable(fake_bin / command, "#!/bin/sh\nexit 0\n")

    environment = os.environ.copy()
    for name in (
        "BCODEX_BUILD_DIR",
        "BCODEX_INSTALL_DIR",
        "BCODEX_RELEASE",
        "BCODEX_REPOSITORY",
        "BCODEX_SOURCE_REVISION",
        "XDG_CACHE_HOME",
    ):
        environment.pop(name, None)
    environment.update(
        {
            "BCODEX_BUILD_DIR": str(build_dir),
            "BCODEX_INSTALL_DIR": str(install_dir),
            "BCODEX_TEST_AUTHENTICATED": "1" if authenticated else "0",
            "BCODEX_TEST_BUILD_LOG": str(root / "build.log"),
            "BCODEX_TEST_BUILD_SUCCESS": "1" if build_success else "0",
            "BCODEX_TEST_BUILT_VERSION": built_version,
            "BCODEX_TEST_BINARY_LOG": str(root / "binary.log"),
            "BCODEX_TEST_COMMIT": COMMIT,
            "BCODEX_TEST_COMMIT_COUNT_FILE": str(root / "commit-count"),
            "BCODEX_TEST_EMBEDDED_REVISION": embedded_revision or "",
            "BCODEX_TEST_GH_LOG": str(root / "gh.log"),
            "BCODEX_TEST_NEXT_COMMIT": next_commit,
            "BCODEX_TEST_REPOSITORY": "ummay0432/bettercodex",
            "BCODEX_TEST_REPOSITORY_ACCESS": "1" if repository_access else "0",
            "BCODEX_TEST_RESOURCE_SMOKE": "1" if resource_smoke_success else "0",
            "BCODEX_TEST_RUNTIME_SMOKE": "1" if runtime_smoke_success else "0",
            "BCODEX_TEST_SOURCE_ARCHIVE": str(archive),
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


def create_source_archive(root: Path, version: str) -> Path:
    archive = root / "source.tar.gz"
    prefix = f"ummay0432-bettercodex-{COMMIT[:7]}"
    files = {
        "Cargo.toml": f'[package]\nname = "bettercodex"\nversion = "{version}"\n',
        "scripts/dev.py": "#!/usr/bin/env python3\n",
    }
    with tarfile.open(archive, "w:gz") as package:
        for relative, contents in files.items():
            encoded = contents.encode()
            entry = tarfile.TarInfo(f"{prefix}/{relative}")
            entry.mode = 0o755 if relative.endswith(".py") else 0o644
            entry.size = len(encoded)
            package.addfile(entry, io.BytesIO(encoded))
    return archive


def write_executable(path: Path, contents: str) -> None:
    path.write_text(contents, encoding="utf-8")
    path.chmod(0o755)


if __name__ == "__main__":
    unittest.main()
