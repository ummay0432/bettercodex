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
INSTALL_COMMAND = INSTALL_SCRIPT.parent.parent.joinpath("INSTALL_COMMAND.txt").read_text(
    encoding="utf-8"
).strip()
CARGO_MANIFEST = INSTALL_SCRIPT.parent.parent / "Cargo.toml"
VERSION = next(
    line.removeprefix('version = "').removesuffix('"')
    for line in CARGO_MANIFEST.read_text(encoding="utf-8").splitlines()
    if line.startswith('version = "')
)
COMMIT = "a" * 40
NEXT_COMMIT = "b" * 40


class InstallScriptTest(unittest.TestCase):
    def test_one_line_bootstrap_fetches_the_canonical_installer_and_cleans_it(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            temporary = root / "temporary"
            temporary.mkdir()
            marker = root / "installer-ran"
            write_executable(
                fake_bin / "gh",
                "#!/bin/sh\n"
                "test \"$*\" = \"api -H Accept: application/vnd.github.raw+json "
                "repos/ummay0432/bettercodex/contents/scripts/install.sh\" || exit 2\n"
                "printf '%s\\n' '#!/bin/sh' "
                "'printf canonical >\"$BCODEX_TEST_BOOTSTRAP_MARKER\"'\n",
            )
            environment = os.environ.copy()
            environment.update(
                {
                    "BCODEX_TEST_BOOTSTRAP_MARKER": str(marker),
                    "PATH": f"{fake_bin}:/usr/bin:/bin",
                    "TMPDIR": str(temporary),
                }
            )

            result = subprocess.run(
                ["/bin/sh", "-c", INSTALL_COMMAND],
                check=False,
                capture_output=True,
                env=environment,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(marker.read_text(encoding="utf-8"), "canonical")
            self.assertEqual(list(temporary.iterdir()), [])

    def test_failed_bootstrap_fetch_does_not_run_or_retain_partial_script(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            temporary = root / "temporary"
            temporary.mkdir()
            write_executable(
                fake_bin / "gh",
                "#!/bin/sh\nprintf '%s\\n' '#!/bin/sh' 'exit 0'\nexit 7\n",
            )
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{fake_bin}:/usr/bin:/bin",
                    "TMPDIR": str(temporary),
                }
            )

            result = subprocess.run(
                ["/bin/sh", "-c", INSTALL_COMMAND],
                check=False,
                capture_output=True,
                env=environment,
                text=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(temporary.iterdir()), [])

    def test_install_and_update_leave_no_source_or_build_cache(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "friend's better codex"
            legacy = root / "cache" / "bettercodex"
            (legacy / "build" / "target" / "release").mkdir(parents=True)
            (legacy / "build" / "target" / "release" / "large-artifact").write_text(
                "retired build output", encoding="utf-8"
            )
            (legacy / "tmp" / "source").mkdir(parents=True)
            (legacy / "rusty-v8-retained-development-cache").mkdir(parents=True)
            (legacy / "keep").write_text("operator data", encoding="utf-8")

            first = run_installer(root)
            second = run_installer(root)

            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertIn("Installed bcodex", first.stdout)
            self.assertIn("Updated bcodex", second.stdout)
            self.assertIn("Removed the temporary source", second.stdout)
            installed = root / "install" / "bin" / "bcodex"
            self.assertEqual(run_binary(installed, "--version"), f"bcodex {VERSION}\n")
            self.assertEqual(run_binary(installed, "--internal-source-revision"), f"{COMMIT}\n")

            profile = (root / "home" / ".profile").read_text(encoding="utf-8")
            self.assertEqual(profile.count("# >>> bettercodex installer >>>"), 1)
            escaped_install_dir = str(installed.parent).replace("'", "'\\''")
            self.assertIn(
                f"export PATH='{escaped_install_dir}':\"$PATH\"",
                profile,
            )
            self.assertFalse((legacy / "build").exists())
            self.assertFalse((legacy / "tmp").exists())
            self.assertTrue((legacy / "rusty-v8-retained-development-cache").is_dir())
            self.assertEqual((legacy / "keep").read_text(encoding="utf-8"), "operator data")

            builds = (root / "build.log").read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(builds), 2)
            for build in builds:
                revision, cargo_home, target, cache_home, compiler_tmp, arguments = build.split(
                    "|", 5
                )
                self.assertEqual(revision, COMMIT)
                self.assertEqual(arguments, "build --release --locked --bin bcodex")
                for disposable_path in (cargo_home, target, cache_home, compiler_tmp):
                    self.assertIn("bettercodex-install.", disposable_path)
                    self.assertFalse(Path(disposable_path).exists())
            assert_no_installer_residue(self, root)

    def test_retries_with_a_fresh_target_when_main_advances(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, next_commit=NEXT_COMMIT)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(
                f"Main advanced from {COMMIT[:12]} to {NEXT_COMMIT[:12]}",
                result.stdout,
            )
            self.assertEqual(
                run_binary(root / "install" / "bin" / "bcodex", "--internal-source-revision"),
                f"{NEXT_COMMIT}\n",
            )
            builds = (root / "build.log").read_text(encoding="utf-8").splitlines()
            self.assertEqual([build.split("|", 1)[0] for build in builds], [COMMIT, NEXT_COMMIT])
            self.assertNotEqual(builds[0].split("|")[2], builds[1].split("|")[2])
            requests = (root / "gh.log").read_text(encoding="utf-8")
            self.assertIn(f"tarball/{COMMIT}", requests)
            self.assertIn(f"tarball/{NEXT_COMMIT}", requests)
            assert_no_installer_residue(self, root)

    def test_failed_build_or_verification_preserves_the_existing_binary(self) -> None:
        cases = (
            ({"build_success": False}, "local BetterCodex compilation failed"),
            ({"embedded_revision": NEXT_COMMIT}, "did not embed source revision"),
            ({"smoke_success": False}, "failed its runtime and embedded-resource smoke test"),
        )
        for options, expected_error in cases:
            with self.subTest(expected_error=expected_error), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                installed = existing_binary(root)
                retired_build = root / "cache" / "bettercodex" / "build" / "target"
                retired_build.mkdir(parents=True)
                (retired_build / "large-artifact").write_text("retired", encoding="utf-8")

                result = run_installer(root, **options)

                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected_error, result.stderr)
                self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
                self.assertFalse(retired_build.parent.exists())
                assert_no_installer_residue(self, root)

    def test_stale_lock_recovers_an_orphaned_temp_tree_and_stage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            install_dir = root / "install" / "bin"
            lock = install_dir / ".bcodex-install.lock"
            orphan = root / "temporary" / "bettercodex-install.orphan"
            lock.mkdir(parents=True)
            orphan.mkdir(parents=True)
            (orphan / "large-artifact").write_text("orphan", encoding="utf-8")
            (lock / "pid").write_text("999999999\n", encoding="utf-8")
            (lock / "tmp").write_text(f"{orphan}\n", encoding="utf-8")
            (install_dir / ".bcodex-stage.orphan").write_text("partial binary", encoding="utf-8")

            result = run_installer(root)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(orphan.exists())
            assert_no_installer_residue(self, root)

    def test_all_supported_native_hosts_use_the_same_exact_source_flow(self) -> None:
        platforms = (
            ("Darwin", "arm64", "macOS ARM64"),
            ("Darwin", "x86_64", "macOS x86-64"),
            ("Linux", "aarch64", "Linux ARM64"),
            ("Linux", "x86_64", "Linux x86-64"),
        )
        for system, machine, label in platforms:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)

                result = run_installer(root, system=system, machine=machine)

                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"for {label}", result.stdout)
                self.assertIn(f"tarball/{COMMIT}", (root / "gh.log").read_text(encoding="utf-8"))
                assert_no_installer_residue(self, root)

    def test_explicit_install_directory_works_without_home(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, home_enabled=False)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Add ", result.stdout)
            self.assertIn(" to PATH in your shell profile", result.stdout)
            self.assertTrue((root / "install" / "bin" / "bcodex").is_file())
            assert_no_installer_residue(self, root)

    def test_missing_pinned_rust_toolchain_is_installed_once(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, toolchain_installed=False)

            self.assertEqual(result.returncode, 0, result.stderr)
            rustup_calls = (root / "rustup.log").read_text(encoding="utf-8")
            self.assertIn("toolchain install 1.95.0 --profile minimal", rustup_calls)
            self.assertTrue((root / "toolchain-installed").is_file())
            assert_no_installer_residue(self, root)


def existing_binary(root: Path) -> Path:
    installed = root / "install" / "bin" / "bcodex"
    installed.parent.mkdir(parents=True)
    installed.write_text("existing binary\n", encoding="utf-8")
    return installed


def run_binary(binary: Path, argument: str) -> str:
    return subprocess.run(
        [binary, argument], check=True, capture_output=True, text=True
    ).stdout


def assert_no_installer_residue(test: unittest.TestCase, root: Path) -> None:
    temporary = root / "temporary"
    if temporary.exists():
        test.assertEqual(list(temporary.glob("bettercodex-install.*")), [])
    install_dir = root / "install" / "bin"
    test.assertFalse((install_dir / ".bcodex-install.lock").exists())
    if install_dir.exists():
        test.assertEqual(list(install_dir.glob(".bcodex-stage.*")), [])


def run_installer(
    root: Path,
    *,
    system: str = "Linux",
    machine: str = "x86_64",
    authenticated: bool = True,
    build_success: bool = True,
    embedded_revision: str | None = None,
    smoke_success: bool = True,
    next_commit: str = COMMIT,
    home_enabled: bool = True,
    toolchain_installed: bool = True,
) -> subprocess.CompletedProcess[str]:
    root.mkdir(parents=True, exist_ok=True)
    fake_bin = root / "fake-bin"
    fake_bin.mkdir(exist_ok=True)
    home = root / "home"
    home.mkdir(exist_ok=True)
    install_dir = root / "install" / "bin"
    temporary = root / "temporary"
    temporary.mkdir(exist_ok=True)
    archive = create_source_archive(root)

    write_executable(
        fake_bin / "uname",
        textwrap.dedent(
            f"""\
            #!/bin/sh
            case "$1" in
              -s) printf '%s\\n' '{system}' ;;
              -m) printf '%s\\n' '{machine}' ;;
              *) exit 2 ;;
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
              api:repos/$BCODEX_TEST_REPOSITORY/commits/main)
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
              api:repos/$BCODEX_TEST_REPOSITORY/tarball/*)
                cat "$BCODEX_TEST_SOURCE_ARCHIVE"
                ;;
              *) exit 2 ;;
            esac
            """
        ),
    )
    for command in ("cargo", "cc", "curl", "rustc"):
        write_executable(fake_bin / command, "#!/bin/sh\nexit 0\n")
    write_executable(
        fake_bin / "rustup",
        "#!/bin/sh\n"
        "printf '%s\\n' \"$*\" >>\"$BCODEX_TEST_RUSTUP_LOG\"\n"
        "case \"$1:$4\" in\n"
        "  toolchain:--profile) : >\"$BCODEX_TEST_TOOLCHAIN_STATE\" ;;\n"
        "  which:cargo)\n"
        "    [ \"$BCODEX_TEST_TOOLCHAIN_INSTALLED\" = 1 ] || "
        "[ -f \"$BCODEX_TEST_TOOLCHAIN_STATE\" ] || exit 3\n"
        "    printf '%s\\n' \"$BCODEX_TEST_FAKE_BIN/cargo\"\n"
        "    ;;\n"
        "  which:rustc)\n"
        "    [ \"$BCODEX_TEST_TOOLCHAIN_INSTALLED\" = 1 ] || "
        "[ -f \"$BCODEX_TEST_TOOLCHAIN_STATE\" ] || exit 3\n"
        "    printf '%s\\n' \"$BCODEX_TEST_FAKE_BIN/rustc\"\n"
        "    ;;\n"
        "  *) exit 2 ;;\n"
        "esac\n",
    )

    environment = os.environ.copy()
    for name in (
        "BCODEX_INSTALL_DIR",
        "BCODEX_REPOSITORY",
        "BCODEX_SOURCE_REVISION",
        "CARGO_BUILD_BUILD_DIR",
        "CARGO_BUILD_TARGET",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_HOME",
        "CARGO_INSTALL_ROOT",
        "CARGO_TARGET_DIR",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTC_WRAPPER",
        "RUSTFLAGS",
        "RUSTUP_TOOLCHAIN",
        "RUSTY_V8_ARCHIVE",
        "RUSTY_V8_SRC_BINDING_PATH",
        "V8_FROM_SOURCE",
        "XDG_CACHE_HOME",
    ):
        environment.pop(name, None)
    environment.update(
        {
            "BCODEX_INSTALL_DIR": str(install_dir),
            "BCODEX_TEST_AUTHENTICATED": "1" if authenticated else "0",
            "BCODEX_TEST_BUILD_LOG": str(root / "build.log"),
            "BCODEX_TEST_BUILD_SUCCESS": "1" if build_success else "0",
            "BCODEX_TEST_BUILT_VERSION": VERSION,
            "BCODEX_TEST_COMMIT": COMMIT,
            "BCODEX_TEST_COMMIT_COUNT_FILE": str(root / "commit-count"),
            "BCODEX_TEST_EMBEDDED_REVISION": embedded_revision or "",
            "BCODEX_TEST_FAKE_BIN": str(fake_bin),
            "BCODEX_TEST_GH_LOG": str(root / "gh.log"),
            "BCODEX_TEST_NEXT_COMMIT": next_commit,
            "BCODEX_TEST_REPOSITORY": "ummay0432/bettercodex",
            "BCODEX_TEST_RUSTUP_LOG": str(root / "rustup.log"),
            "BCODEX_TEST_SMOKE_SUCCESS": "1" if smoke_success else "0",
            "BCODEX_TEST_SOURCE_ARCHIVE": str(archive),
            "BCODEX_TEST_TOOLCHAIN_INSTALLED": "1" if toolchain_installed else "0",
            "BCODEX_TEST_TOOLCHAIN_STATE": str(root / "toolchain-installed"),
            "PATH": f"{fake_bin}:/usr/bin:/bin",
            "SHELL": "/bin/sh",
            "TMPDIR": str(temporary),
            "XDG_CACHE_HOME": str(root / "cache"),
        }
    )
    if home_enabled:
        environment["HOME"] = str(home)
    else:
        environment.pop("HOME", None)

    return subprocess.run(
        ["/bin/sh", INSTALL_SCRIPT],
        check=False,
        capture_output=True,
        env=environment,
        text=True,
    )


def create_source_archive(root: Path) -> Path:
    archive = root / "source.tar.gz"
    prefix = f"ummay0432-bettercodex-{COMMIT[:7]}"
    files = {
        "Cargo.toml": f'[package]\nname = "bettercodex"\nversion = "{VERSION}"\n',
        "Cargo.lock": "# fixture lockfile\n",
        "rust-toolchain.toml": '[toolchain]\nchannel = "1.95.0"\n',
        "scripts/cargo-with-v8.sh": textwrap.dedent(
            """\
            #!/bin/sh
            printf '%s|%s|%s|%s|%s|%s\\n' \
              "$BCODEX_SOURCE_REVISION" "$CARGO_HOME" "$CARGO_TARGET_DIR" \
              "$XDG_CACHE_HOME" "$TMPDIR" "$*" >>"$BCODEX_TEST_BUILD_LOG"
            [ "$BCODEX_TEST_BUILD_SUCCESS" = "1" ] || exit 9
            mkdir -p "$CARGO_TARGET_DIR/release"
            {
              printf '%s\\n' '#!/bin/sh'
              printf 'version=%s\\n' "$BCODEX_TEST_BUILT_VERSION"
              printf 'source_revision=%s\\n' "${BCODEX_TEST_EMBEDDED_REVISION:-$BCODEX_SOURCE_REVISION}"
              printf 'smoke_success=%s\\n' "$BCODEX_TEST_SMOKE_SUCCESS"
              printf '%s\\n' 'case "${1:-}" in'
              printf '%s\\n' '  --version) printf "bcodex %s\\n" "$version" ;;'
              printf '%s\\n' '  --internal-source-revision) printf "%s\\n" "$source_revision" ;;'
              printf '%s\\n' '  --internal-install-smoke)'
              printf '%s\\n' '    [ "$smoke_success" = 1 ] || exit 12'
              printf '%s\\n' '    printf "bcodex %s install smoke passed\\n" "$version"'
              printf '%s\\n' '    ;;'
              printf '%s\\n' '  *) exit 13 ;;'
              printf '%s\\n' 'esac'
            } >"$CARGO_TARGET_DIR/release/bcodex"
            chmod 0755 "$CARGO_TARGET_DIR/release/bcodex"
            """
        ),
    }
    with tarfile.open(archive, "w:gz") as package:
        for relative, contents in files.items():
            data = contents.encode("utf-8")
            info = tarfile.TarInfo(f"{prefix}/{relative}")
            info.size = len(data)
            info.mode = 0o755 if relative == "scripts/cargo-with-v8.sh" else 0o644
            info.mtime = 0
            package.addfile(info, io.BytesIO(data))
    return archive


def write_executable(path: Path, contents: str) -> None:
    path.write_text(contents, encoding="utf-8")
    path.chmod(0o755)


if __name__ == "__main__":
    unittest.main()
