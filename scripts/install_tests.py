#!/usr/bin/env python3

from __future__ import annotations

import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import textwrap
import unittest


INSTALL_SCRIPT = Path(__file__).with_name("install.sh")
INSTALL_COMMAND = INSTALL_SCRIPT.parent.parent.joinpath("INSTALL_COMMAND.txt").read_text(
    encoding="utf-8"
).strip()
README = INSTALL_SCRIPT.parent.parent.joinpath("README.md").read_text(encoding="utf-8")
CARGO_MANIFEST = INSTALL_SCRIPT.parent.parent / "Cargo.toml"
VERSION = next(
    line.removeprefix('version = "').removesuffix('"')
    for line in CARGO_MANIFEST.read_text(encoding="utf-8").splitlines()
    if line.startswith('version = "')
)
COMMIT = "a" * 40
NEXT_COMMIT = "b" * 40


class InstallScriptTest(unittest.TestCase):
    def test_readme_exposes_the_canonical_copyable_install_command(self) -> None:
        self.assertIn(f"```sh\n{INSTALL_COMMAND}\n```", README)

    def test_one_line_bootstrap_fetches_and_runs_the_canonical_installer(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            marker = root / "installer-ran"
            write_executable(
                fake_bin / "curl",
                "#!/bin/sh\n"
                "[ \"$#\" -eq 2 ] && [ \"$1\" = '-fsSL' ] && "
                "[ \"$2\" = "
                "'https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.sh' ] "
                "|| exit 2\n"
                "printf '%s\\n' '#!/bin/sh' "
                "'printf canonical >\"$BCODEX_TEST_BOOTSTRAP_MARKER\"'\n",
            )
            environment = os.environ.copy()
            environment.update(
                {
                    "BCODEX_TEST_BOOTSTRAP_MARKER": str(marker),
                    "PATH": f"{fake_bin}:/usr/bin:/bin",
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

    def test_install_and_update_reuse_downloads_and_cargo_outputs(self) -> None:
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
            self.assertIn("retained the warm Cargo cache", second.stdout)
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
            self.assertFalse((legacy / "build" / "target").exists())
            self.assertFalse((legacy / "tmp").exists())
            self.assertTrue((legacy / "rusty-v8-retained-development-cache").is_dir())
            self.assertEqual((legacy / "keep").read_text(encoding="utf-8"), "operator data")

            builds = (root / "build.log").read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(builds), 2)
            expected_cargo_home = legacy / "cargo"
            expected_cache_home = root / "cache"
            expected_target = legacy / "build" / "x86_64-unknown-linux-gnu" / "target"
            build_input_hashes = []
            for build in builds:
                (
                    incremental,
                    cargo_home,
                    target,
                    cache_home,
                    compiler_tmp,
                    build_input_hash,
                    arguments,
                ) = build.split("|", 6)
                self.assertEqual(incremental, "1")
                self.assertRegex(build_input_hash, r"^[0-9a-f]{64}$")
                build_input_hashes.append(build_input_hash)
                self.assertEqual(arguments, "build --release --locked --bin bcodex")
                self.assertEqual(Path(cargo_home), expected_cargo_home)
                self.assertEqual(Path(cache_home), expected_cache_home)
                self.assertEqual(Path(target), expected_target)
                self.assertIn("bettercodex-install.", compiler_tmp)
                self.assertFalse(Path(compiler_tmp).exists())
            self.assertEqual(build_input_hashes[0], build_input_hashes[1])
            self.assertTrue(expected_cargo_home.is_dir())
            self.assertTrue((expected_target / "release" / "deps" / "fixture-dependency.rlib").is_file())
            self.assertTrue((expected_target / "release" / "bcodex").is_file())
            self.assertEqual(
                (root / "compile.log").read_text(encoding="utf-8").splitlines(),
                ["dependency"],
            )
            self.assertEqual(
                (root / "download.log").read_text(encoding="utf-8").splitlines(),
                ["cargo", "v8"],
            )
            assert_no_installer_residue(self, root)

    def test_symlinked_destination_is_refused_before_network_or_build_work(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            install_dir = root / "install" / "bin"
            install_dir.mkdir(parents=True)
            outside = root / "outside-binary"
            outside.write_text("operator data\n", encoding="utf-8")
            (install_dir / "bcodex").symlink_to(outside)

            result = run_installer(root)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("refusing to replace symlinked", result.stderr)
            self.assertEqual(outside.read_text(encoding="utf-8"), "operator data\n")
            self.assertFalse((root / "github.log").exists())
            self.assertFalse((root / "build.log").exists())

    def test_source_install_builds_the_once_resolved_immutable_commit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, next_commit=NEXT_COMMIT)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                run_binary(root / "install" / "bin" / "bcodex", "--internal-source-revision"),
                f"{COMMIT}\n",
            )
            builds = (root / "build.log").read_text(encoding="utf-8").splitlines()
            self.assertEqual([build.split("|", 1)[0] for build in builds], ["1"])
            self.assertEqual(
                (root / "compile.log").read_text(encoding="utf-8").splitlines(),
                ["dependency"],
            )
            requests = (root / "github.log").read_text(encoding="utf-8")
            self.assertIn(f"tar.gz/{COMMIT}", requests)
            self.assertNotIn(f"tar.gz/{NEXT_COMMIT}", requests)
            assert_no_installer_residue(self, root)

    def test_failed_build_or_verification_preserves_the_existing_binary(self) -> None:
        cases = (
            ({"build_success": False}, "local bettercodex compilation failed"),
            (
                {"embedded_build_input_hash": "b" * 64},
                "built binary could not stage source revision",
            ),
            ({"embedded_revision": NEXT_COMMIT}, "staged binary lost its embedded source revision"),
            ({"smoke_success": False}, "staged binary failed its runtime and embedded-resource smoke test"),
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
                self.assertFalse(retired_build.exists())
                self.assertTrue(
                    (
                        retired_build.parent
                        / "x86_64-unknown-linux-gnu"
                        / "target"
                        / "release"
                        / "deps"
                        / "fixture-dependency.rlib"
                    ).is_file()
                )
                assert_no_installer_residue(self, root)

    def test_stale_lock_recovers_an_orphaned_temp_tree_and_stage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            install_dir = root / "install" / "bin"
            lock = install_dir / ".bcodex-install.lock"
            old_temporary = root / "old temporary"
            orphan = old_temporary / "bettercodex-install.orphan"
            lock.mkdir(parents=True)
            orphan.mkdir(parents=True)
            (orphan / "large-artifact").write_text("orphan", encoding="utf-8")
            (lock / "pid").write_text("999999999\n", encoding="utf-8")
            (lock / "tmp").write_text(f"{orphan}\n", encoding="utf-8")
            (lock / "tmp-parent").write_text(f"{old_temporary}\n", encoding="utf-8")
            (install_dir / ".bcodex-stage.orphan").write_text("partial binary", encoding="utf-8")

            result = run_installer(root)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(orphan.exists())
            assert_no_installer_residue(self, root)

    def test_all_supported_native_hosts_use_the_same_exact_source_flow(self) -> None:
        platforms = (
            ("Darwin", "arm64", "/bin/zsh", ".zprofile", "macOS ARM64", "aarch64-apple-darwin"),
            ("Darwin", "x86_64", "/bin/zsh", ".zprofile", "macOS x86-64", "x86_64-apple-darwin"),
            ("Linux", "aarch64", "/bin/bash", ".bashrc", "Linux ARM64", "aarch64-unknown-linux-gnu"),
            ("Linux", "x86_64", "/bin/bash", ".bashrc", "Linux x86-64", "x86_64-unknown-linux-gnu"),
        )
        for system, machine, shell, profile, label, host_target in platforms:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)

                result = run_installer(root, system=system, machine=machine, shell=shell)

                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"for {label}", result.stdout)
                self.assertIn(
                    f"tar.gz/{COMMIT}",
                    (root / "github.log").read_text(encoding="utf-8"),
                )
                self.assertTrue((root / "home" / profile).is_file())
                build = (root / "build.log").read_text(encoding="utf-8").strip().split("|")
                self.assertEqual(
                    Path(build[2]),
                    root / "cache" / "bettercodex" / "build" / host_target / "target",
                )
                codesign_log = root / "codesign.log"
                if system == "Darwin":
                    codesign_calls = codesign_log.read_text(encoding="utf-8").splitlines()
                    self.assertEqual(len(codesign_calls), 2)
                    self.assertTrue(codesign_calls[0].startswith("--force --sign - "))
                    self.assertTrue(codesign_calls[1].startswith("--verify --strict "))
                else:
                    self.assertFalse(codesign_log.exists())
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

    def test_home_cache_is_used_when_xdg_cache_home_is_unset(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, xdg_cache_enabled=False)

            self.assertEqual(result.returncode, 0, result.stderr)
            build = (root / "build.log").read_text(encoding="utf-8").strip().split("|")
            self.assertEqual(
                Path(build[1]), root / "home" / ".cache" / "bettercodex" / "cargo"
            )
            self.assertEqual(
                Path(build[2]),
                root
                / "home"
                / ".cache"
                / "bettercodex"
                / "build"
                / "x86_64-unknown-linux-gnu"
                / "target",
            )
            self.assertEqual(Path(build[3]), root / "home" / ".cache")
            assert_no_installer_residue(self, root)

    def test_missing_cache_environment_uses_disposable_downloads(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, home_enabled=False, xdg_cache_enabled=False)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("dependency downloads cannot be reused", result.stderr)
            build = (root / "build.log").read_text(encoding="utf-8").strip().split("|")
            for disposable_path in (build[1], build[2], build[3], build[4]):
                self.assertIn("bettercodex-install.", disposable_path)
                self.assertFalse(Path(disposable_path).exists())
            assert_no_installer_residue(self, root)

    def test_rust_bootstrap_is_disposable_without_home_or_cache(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(
                root,
                home_enabled=False,
                rustup_location="absent",
                toolchain_installed=False,
                xdg_cache_enabled=False,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            bootstrap = (root / "rustup-bootstrap.log").read_text(
                encoding="utf-8"
            ).strip()
            cargo_home, rustup_home, _arguments = bootstrap.split("|", 2)
            for disposable_path in (cargo_home, rustup_home):
                self.assertIn("bettercodex-install.", disposable_path)
                self.assertFalse(Path(disposable_path).exists())
            self.assertIn("optional Rust toolchain", result.stdout)
            assert_no_installer_residue(self, root)

    def test_missing_pinned_rust_toolchain_is_cached_once_for_updates(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            first = run_installer(root, toolchain_installed=False)
            second = run_installer(root, toolchain_installed=False)

            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(second.returncode, 0, second.stderr)
            rustup_calls = (root / "rustup.log").read_text(encoding="utf-8").splitlines()
            installs = [
                call.split("|", 1)
                for call in rustup_calls
                if "toolchain install" in call
            ]
            self.assertEqual(len(installs), 1)
            install_home, install_arguments = installs[0]
            self.assertEqual(
                install_arguments,
                "toolchain install 1.95.0 --profile minimal",
            )
            self.assertEqual(
                Path(install_home), root / "cache" / "bettercodex" / "rustup"
            )
            self.assertTrue(Path(install_home).is_dir())
            self.assertIn("retained warm Rust and Cargo caches", second.stdout)
            assert_no_installer_residue(self, root)

    def test_rustup_installed_but_not_on_path_is_used_immediately(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, rustup_location="home")

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("even though it is not on PATH", result.stdout)
            self.assertFalse((root / "rustup-bootstrap.log").exists())
            assert_no_installer_residue(self, root)

    def test_missing_rustup_and_toolchain_are_bootstrapped_and_cached_once(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            first = run_installer(
                root, rustup_location="absent", toolchain_installed=False
            )
            second = run_installer(
                root, rustup_location="absent", toolchain_installed=False
            )

            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(second.returncode, 0, second.stderr)
            bootstrap = (root / "rustup-bootstrap.log").read_text(
                encoding="utf-8"
            ).splitlines()
            self.assertEqual(len(bootstrap), 1)
            cargo_home, rustup_home, arguments = bootstrap[0].split("|", 2)
            self.assertEqual(
                Path(cargo_home), root / "cache" / "bettercodex" / "cargo"
            )
            self.assertEqual(
                Path(rustup_home), root / "cache" / "bettercodex" / "rustup"
            )
            self.assertEqual(
                arguments,
                "-y --no-modify-path --profile minimal --default-toolchain none",
            )
            rustup_calls = (root / "rustup.log").read_text(
                encoding="utf-8"
            ).splitlines()
            self.assertEqual(
                sum("toolchain install" in call for call in rustup_calls), 1
            )
            self.assertIn("Reusing the installer-managed rustup", second.stdout)
            assert_no_installer_residue(self, root)

    def test_failed_rustup_bootstrap_preserves_the_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(
                root,
                rustup_bootstrap_success=False,
                rustup_location="absent",
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("the official rustup installer failed", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
            self.assertFalse((root / "build.log").exists())
            assert_no_installer_residue(self, root)

    def test_known_revision_skips_the_redundant_initial_main_lookup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, install_revision=COMMIT)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((root / "main-request-count").exists())
            self.assertEqual(
                run_binary(root / "install" / "bin" / "bcodex", "--internal-source-revision"),
                f"{COMMIT}\n",
            )
            assert_no_installer_residue(self, root)

    def test_failed_source_hash_uses_revision_specific_freshness(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, source_hash_success=False)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("using conservative per-revision freshness", result.stderr)
            build = (root / "build.log").read_text(encoding="utf-8").strip().split("|")
            self.assertEqual(build[-2], COMMIT + COMMIT[:24])
            assert_no_installer_residue(self, root)

    def test_manifest_and_lockfile_changes_keep_cargo_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            first = run_installer(root, source_generation="first")
            identity = (
                root
                / "cache"
                / "bettercodex"
                / "build"
                / "x86_64-unknown-linux-gnu"
                / "identity"
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            identity.write_text(
                "bettercodex-build-cache-v1\n"
                "host=x86_64-unknown-linux-gnu\n"
                "toolchain=old-toolchain-hash\n"
                "manifest=old-manifest-hash\n"
                "lockfile=old-lockfile-hash\n"
                "v8-wrapper=old-wrapper-hash\n",
                encoding="utf-8",
            )

            second = run_installer(root, source_generation="other")

            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertNotIn("Resetting", second.stdout)
            self.assertFalse(identity.exists())
            self.assertEqual(
                (root / "compile.log").read_text(encoding="utf-8").splitlines(),
                ["dependency"],
            )
            build_input_hashes = [
                build.split("|")[-2]
                for build in (root / "build.log").read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(len(build_input_hashes), 2)
            self.assertNotEqual(build_input_hashes[0], build_input_hashes[1])
            assert_no_installer_residue(self, root)

    def test_transient_github_failures_retry_without_mixing_partial_archives(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, main_failures=2, archive_failures=2)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("GitHub request failed; retrying (2/3)", result.stderr)
            self.assertIn("GitHub source download failed; retrying (3/3)", result.stderr)
            self.assertEqual(
                (root / "main-request-count").read_text(encoding="utf-8").strip(),
                "3",
            )
            self.assertEqual(
                (root / "archive-request-count").read_text(encoding="utf-8").strip(),
                "3",
            )
            self.assertEqual(
                run_binary(root / "install" / "bin" / "bcodex", "--internal-source-revision"),
                f"{COMMIT}\n",
            )
            assert_no_installer_residue(self, root)

    def test_exhausted_github_retries_preserve_the_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(root, main_failures=3)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("could not resolve the current bettercodex main commit", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
            self.assertFalse((root / "build.log").exists())
            assert_no_installer_residue(self, root)

    def test_fresh_linux_installs_native_build_tools_and_continues(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, compiler_works=False)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                (root / "system-packages.log").read_text(encoding="utf-8").splitlines(),
                ["update", "install -y build-essential"],
            )
            self.assertEqual(
                (root / "sudo.log").read_text(encoding="utf-8").splitlines(),
                ["apt-get update", "apt-get install -y build-essential"],
            )
            self.assertIn("Installing the native build tools", result.stdout)
            self.assertTrue((root / "install" / "bin" / "bcodex").is_file())
            assert_no_installer_residue(self, root)

    def test_supported_linux_package_managers_install_native_build_tools(self) -> None:
        cases = {
            "dnf": "-y group install Development Tools",
            "yum": "-y groupinstall Development Tools",
            "zypper": "--non-interactive install --type pattern devel_basis",
            "pacman": "-S --needed --noconfirm base-devel",
            "xbps-install": "-Sy base-devel",
        }
        for package_manager, expected_arguments in cases.items():
            with self.subTest(package_manager=package_manager), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)

                result = run_installer(
                    root,
                    compiler_works=False,
                    package_manager=package_manager,
                )

                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    (root / "system-packages.log").read_text(encoding="utf-8").strip(),
                    expected_arguments,
                )
                self.assertEqual(
                    (root / "sudo.log").read_text(encoding="utf-8").strip(),
                    f"{package_manager} {expected_arguments}",
                )
                assert_no_installer_residue(self, root)

    def test_failed_native_build_tool_bootstrap_preserves_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(
                root,
                compiler_works=False,
                build_tools_install_success=False,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("automatic native build-tool installation failed", result.stderr)
            self.assertIn("apt-get install -y build-essential", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
            self.assertFalse((root / "build.log").exists())
            assert_no_installer_residue(self, root)

    def test_unknown_linux_package_manager_gets_an_actionable_remedy(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(
                root,
                compiler_works=False,
                package_manager="none",
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("C/C++ compiler, linker, and libc development headers", result.stderr)
            self.assertFalse((root / "build.log").exists())
            assert_no_installer_residue(self, root)

    def test_fresh_macos_requests_command_line_tools_before_stopping(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, system="Darwin", machine="arm64", compiler_works=False)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("finish the macOS installation dialog", result.stderr)
            self.assertEqual(
                (root / "xcode-select.log").read_text(encoding="utf-8").strip(),
                "--install",
            )
            self.assertFalse((root / "build.log").exists())
            assert_no_installer_residue(self, root)

    def test_failed_macos_signing_preserves_the_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(
                root,
                system="Darwin",
                machine="arm64",
                codesign_success=False,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("could not apply the required macOS ad-hoc signature", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
            assert_no_installer_residue(self, root)

    def test_an_older_bcodex_earlier_on_path_is_reported_and_overridden(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, shadowed_binary=True)

            self.assertEqual(result.returncode, 0, result.stderr)
            shadow = root / "fake-bin" / "bcodex"
            installed = root / "install" / "bin" / "bcodex"
            self.assertIn(f"{shadow} currently shadows", result.stderr)
            profile = (root / "home" / ".profile").read_text(encoding="utf-8")
            self.assertIn(f"export PATH='{installed.parent}':\"$PATH\"", profile)
            self.assertIn("For this terminal: export PATH=", result.stdout)
            assert_no_installer_residue(self, root)

    def test_path_block_update_preserves_private_profile_permissions(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / "home"
            home.mkdir(parents=True)
            profile = home / ".profile"
            profile.write_text(
                "operator setting\n"
                "# >>> bettercodex installer >>>\n"
                "export PATH='/old/install':\"$PATH\"\n"
                "# <<< bettercodex installer <<<\n",
                encoding="utf-8",
            )
            profile.chmod(0o600)

            result = run_installer(root)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(profile.stat().st_mode & 0o777, 0o600)
            self.assertIn("operator setting", profile.read_text(encoding="utf-8"))
            self.assertIn(str(root / "install" / "bin"), profile.read_text(encoding="utf-8"))
            assert_no_installer_residue(self, root)

    def test_path_block_update_does_not_replace_a_symlinked_profile(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / "home"
            home.mkdir(parents=True)
            profile_target = root / "managed-profile"
            original = (
                "# >>> bettercodex installer >>>\n"
                "export PATH='/old/install':\"$PATH\"\n"
                "# <<< bettercodex installer <<<\n"
            )
            profile_target.write_text(original, encoding="utf-8")
            profile = home / ".profile"
            profile.symlink_to(profile_target)

            result = run_installer(root)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(profile.is_symlink())
            self.assertEqual(profile_target.read_text(encoding="utf-8"), original)
            self.assertIn("not replacing symlinked shell profile", result.stderr)
            self.assertIn("Add ", result.stdout)
            self.assertIn(" to PATH in your shell profile", result.stdout)
            assert_no_installer_residue(self, root)

    def test_compiled_cache_never_follows_a_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            outside = root / "operator-cache"
            (outside / "target").mkdir(parents=True)
            keep = outside / "target" / "keep"
            keep.write_text("operator data", encoding="utf-8")
            legacy = root / "cache" / "bettercodex"
            legacy.mkdir(parents=True)
            (legacy / "build").symlink_to(outside, target_is_directory=True)

            result = run_installer(root)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("is not a regular directory; using disposable output", result.stderr)
            self.assertTrue((legacy / "build").is_symlink())
            self.assertEqual(keep.read_text(encoding="utf-8"), "operator data")
            assert_no_installer_residue(self, root)

    def test_cache_root_symlink_is_never_followed_for_cleanup_or_builds(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            outside = root / "operator-cache"
            (outside / "build" / "target").mkdir(parents=True)
            keep = outside / "build" / "target" / "keep"
            keep.write_text("operator data", encoding="utf-8")
            cache_base = root / "cache"
            cache_base.mkdir()
            (cache_base / "bettercodex").symlink_to(outside, target_is_directory=True)

            result = run_installer(root)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("cache root", result.stderr)
            self.assertIn("using disposable caches", result.stderr)
            self.assertEqual(keep.read_text(encoding="utf-8"), "operator data")
            self.assertTrue((cache_base / "bettercodex").is_symlink())
            build = (root / "build.log").read_text(encoding="utf-8").strip().split("|")
            self.assertIn("bettercodex-install.", build[2])
            assert_no_installer_residue(self, root)

    def test_invalid_requested_revision_stops_before_network_or_build_work(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, install_revision="not-a-commit")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("BCODEX_INSTALL_REVISION must be a full", result.stderr)
            self.assertFalse((root / "github.log").exists())
            self.assertFalse((root / "build.log").exists())
            assert_no_installer_residue(self, root)

    def test_failed_exit_cleanup_keeps_a_record_for_the_next_install(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            failed = run_installer(root, build_success=False, cleanup_success=False)

            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("retaining installer lock", failed.stderr)
            lock = root / "install" / "bin" / ".bcodex-install.lock"
            orphan = Path((lock / "tmp").read_text(encoding="utf-8").strip())
            self.assertTrue(orphan.is_dir())

            recovered = run_installer(root)

            self.assertEqual(recovered.returncode, 0, recovered.stderr)
            self.assertFalse(orphan.exists())
            assert_no_installer_residue(self, root)

    def test_main_advancing_during_a_build_does_not_repeat_the_large_build(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(root, advancing_main=True, next_commit=NEXT_COMMIT)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
            self.assertEqual(run_binary(installed, "--internal-source-revision"), f"{COMMIT}\n")
            builds = (root / "build.log").read_text(encoding="utf-8").splitlines()
            self.assertEqual(
                [build.split("|", 1)[0] for build in builds],
                ["1"],
            )
            assert_no_installer_residue(self, root)

    def test_active_install_lock_rejects_a_concurrent_build(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lock = root / "install" / "bin" / ".bcodex-install.lock"
            lock.mkdir(parents=True)
            (lock / "pid").write_text(f"{os.getpid()}\n", encoding="utf-8")

            result = run_installer(root)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("another bettercodex install is already running", result.stderr)
            self.assertTrue(lock.is_dir())
            self.assertFalse((root / "build.log").exists())


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
    archive_failures: int = 0,
    build_success: bool = True,
    build_tools_install_success: bool = True,
    codesign_success: bool = True,
    compiler_works: bool = True,
    embedded_build_input_hash: str | None = None,
    embedded_revision: str | None = None,
    install_revision: str | None = None,
    main_failures: int = 0,
    smoke_success: bool = True,
    next_commit: str = COMMIT,
    home_enabled: bool = True,
    package_manager: str = "apt-get",
    rustup_bootstrap_success: bool = True,
    rustup_location: str = "path",
    toolchain_installed: bool = True,
    shell: str = "/bin/sh",
    cleanup_success: bool = True,
    advancing_main: bool = False,
    shadowed_binary: bool = False,
    source_hash_success: bool = True,
    source_generation: str = "first",
    xdg_cache_enabled: bool = True,
) -> subprocess.CompletedProcess[str]:
    if package_manager not in {
        "apt-get",
        "dnf",
        "none",
        "pacman",
        "xbps-install",
        "yum",
        "zypper",
    }:
        raise ValueError(f"unsupported package manager fixture: {package_manager}")
    if rustup_location not in {"path", "home", "absent"}:
        raise ValueError(f"unsupported rustup fixture location: {rustup_location}")
    root.mkdir(parents=True, exist_ok=True)
    fake_bin = root / "fake-bin"
    fake_bin.mkdir(exist_ok=True)
    system_bin = root / "system-bin"
    system_bin.mkdir(exist_ok=True)
    for command in (
        "awk",
        "cat",
        "chmod",
        "cp",
        "dirname",
        "find",
        "grep",
        "gzip",
        "mkdir",
        "mktemp",
        "mv",
        "sed",
        "sha256sum",
        "sort",
        "tar",
        "tr",
    ):
        program = shutil.which(command)
        if program is None:
            raise RuntimeError(f"installer test host has no {command}")
        command_path = system_bin / command
        if not command_path.exists():
            command_path.symlink_to(program)
    home = root / "home"
    home.mkdir(exist_ok=True)
    install_dir = root / "install" / "bin"
    temporary = root / "temporary"
    temporary.mkdir(exist_ok=True)
    archive = create_source_archive(root, source_generation)

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
        fake_bin / "curl",
        textwrap.dedent(
            """\
            #!/bin/sh
            printf '%s\\n' "$*" >>"$BCODEX_TEST_GITHUB_LOG"
            output=""
            write_status=0
            next_output=0
            next_write=0
            url=""
            for argument do
              if [ "$next_output" = 1 ]; then
                output="$argument"
                next_output=0
                continue
              fi
              if [ "$next_write" = 1 ]; then
                next_write=0
                continue
              fi
              case "$argument" in
                --output) next_output=1 ;;
                --write-out) write_status=1; next_write=1 ;;
                https://*) url="$argument" ;;
              esac
            done

            emit_status() {
              [ "$write_status" = 1 ] && printf '%s' "$1"
              return 0
            }

            emit_empty() {
              [ -z "$output" ] || : >"$output"
              emit_status "$1"
            }

            emit_file() {
              if [ -n "$output" ]; then
                cat "$1" >"$output"
              else
                cat "$1"
              fi
              emit_status "$2"
            }

            case "$url" in
              "https://sh.rustup.rs")
                emit_file "$BCODEX_TEST_RUSTUP_INSTALLER" 200
                ;;
              "https://api.github.com/repos/$BCODEX_TEST_REPOSITORY/git/ref/heads/main")
                count=0
                if [ -f "$BCODEX_TEST_MAIN_REQUEST_COUNT_FILE" ]; then
                  count="$(sed -n '1p' "$BCODEX_TEST_MAIN_REQUEST_COUNT_FILE")"
                fi
                count=$((count + 1))
                printf '%s\\n' "$count" >"$BCODEX_TEST_MAIN_REQUEST_COUNT_FILE"
                if [ "$count" -le "$BCODEX_TEST_MAIN_FAILURES" ]; then
                  exit 75
                fi
                success_count=$((count - BCODEX_TEST_MAIN_FAILURES))
                if [ "$BCODEX_TEST_ADVANCING_MAIN" = 1 ]; then
                  case "$success_count" in
                    1) revision="$BCODEX_TEST_COMMIT" ;;
                    2 | 3) revision="$BCODEX_TEST_NEXT_COMMIT" ;;
                    4 | 5) revision="$(printf '%040d\\n' 0 | tr 0 c)" ;;
                    *) revision="$(printf '%040d\\n' 0 | tr 0 d)" ;;
                  esac
                elif [ "$success_count" -eq 1 ]; then
                  revision="$BCODEX_TEST_COMMIT"
                else
                  revision="$BCODEX_TEST_NEXT_COMMIT"
                fi
                printf '{"ref":"refs/heads/main","object":{"sha":"%s","type":"commit"}}\\n' \
                  "$revision"
                ;;
              "https://codeload.github.com/$BCODEX_TEST_REPOSITORY/tar.gz/"*)
                count=0
                if [ -f "$BCODEX_TEST_ARCHIVE_REQUEST_COUNT_FILE" ]; then
                  count="$(sed -n '1p' "$BCODEX_TEST_ARCHIVE_REQUEST_COUNT_FILE")"
                fi
                count=$((count + 1))
                printf '%s\\n' "$count" >"$BCODEX_TEST_ARCHIVE_REQUEST_COUNT_FILE"
                if [ "$count" -le "$BCODEX_TEST_ARCHIVE_FAILURES" ]; then
                  printf 'partial archive'
                  exit 75
                fi
                cat "$BCODEX_TEST_SOURCE_ARCHIVE"
                ;;
              *) exit 2 ;;
            esac
            """
        ),
    )
    for command in ("cargo", "rustc"):
        write_executable(fake_bin / command, "#!/bin/sh\nexit 0\n")
    for command in ("cc", "c++"):
        write_executable(
            fake_bin / command,
            "#!/bin/sh\n"
            "[ \"$BCODEX_TEST_COMPILER_WORKS\" = 1 ] || "
            "[ -f \"$BCODEX_TEST_COMPILER_INSTALLED_FILE\" ]\n",
        )
    write_executable(fake_bin / "id", "#!/bin/sh\nprintf '1000\\n'\n")
    write_executable(
        fake_bin / "sudo",
        "#!/bin/sh\n"
        "printf '%s\\n' \"$*\" >>\"$BCODEX_TEST_SUDO_LOG\"\n"
        "exec \"$@\"\n",
    )
    if package_manager != "none":
        write_executable(
            fake_bin / package_manager,
            "#!/bin/sh\n"
            "printf '%s\\n' \"$*\" >>\"$BCODEX_TEST_SYSTEM_PACKAGES_LOG\"\n"
            "[ \"$BCODEX_TEST_BUILD_TOOLS_INSTALL_SUCCESS\" = 1 ] || exit 9\n"
            ": >\"$BCODEX_TEST_COMPILER_INSTALLED_FILE\"\n",
        )
    write_executable(
        fake_bin / "xcode-select",
        "#!/bin/sh\n"
        "printf '%s\\n' \"$*\" >>\"$BCODEX_TEST_XCODE_SELECT_LOG\"\n"
        "exit 0\n",
    )
    write_executable(
        fake_bin / "codesign",
        "#!/bin/sh\n"
        "printf '%s\\n' \"$*\" >>\"$BCODEX_TEST_CODESIGN_LOG\"\n"
        "[ \"$BCODEX_TEST_CODESIGN_SUCCESS\" = 1 ]\n",
    )
    write_executable(fake_bin / "sleep", "#!/bin/sh\nexit 0\n")
    if not source_hash_success:
        write_executable(fake_bin / "sha256sum", "#!/bin/sh\nexit 1\n")
    if shadowed_binary:
        write_executable(
            fake_bin / "bcodex",
            "#!/bin/sh\nprintf 'bcodex 0.0.1\\n'\n",
        )
    write_executable(
        fake_bin / "rm",
        "#!/bin/sh\n"
        "if [ \"$BCODEX_TEST_CLEANUP_SUCCESS\" != 1 ]; then\n"
        "  for argument in \"$@\"; do\n"
        "    case \"$argument\" in *bettercodex-install.*) exit 9 ;; esac\n"
        "  done\n"
        "fi\n"
        "exec /bin/rm \"$@\"\n",
    )
    rustup_fixture = root / "rustup-fixture"
    write_executable(
        rustup_fixture,
        "#!/bin/sh\n"
        "printf '%s|%s\\n' \"${RUSTUP_HOME:-<default>}\" \"$*\" "
        ">>\"$BCODEX_TEST_RUSTUP_LOG\"\n"
        "case \"$1:$4\" in\n"
        "  toolchain:--profile)\n"
        "    [ -n \"${RUSTUP_HOME:-}\" ] || exit 4\n"
        "    mkdir -p \"$RUSTUP_HOME\"\n"
        "    : >\"$RUSTUP_HOME/toolchain-installed\"\n"
        "    ;;\n"
        "  which:cargo)\n"
        "    if [ -n \"${RUSTUP_HOME:-}\" ]; then\n"
        "      [ -f \"$RUSTUP_HOME/toolchain-installed\" ] || exit 3\n"
        "    else\n"
        "      [ \"$BCODEX_TEST_TOOLCHAIN_INSTALLED\" = 1 ] || exit 3\n"
        "    fi\n"
        "    printf '%s\\n' \"$BCODEX_TEST_FAKE_BIN/cargo\"\n"
        "    ;;\n"
        "  which:rustc)\n"
        "    if [ -n \"${RUSTUP_HOME:-}\" ]; then\n"
        "      [ -f \"$RUSTUP_HOME/toolchain-installed\" ] || exit 3\n"
        "    else\n"
        "      [ \"$BCODEX_TEST_TOOLCHAIN_INSTALLED\" = 1 ] || exit 3\n"
        "    fi\n"
        "    printf '%s\\n' \"$BCODEX_TEST_FAKE_BIN/rustc\"\n"
        "    ;;\n"
        "  *) exit 2 ;;\n"
        "esac\n",
    )
    write_executable(
        root / "rustup-init-fixture",
        "#!/bin/sh\n"
        "printf '%s|%s|%s\\n' \"$CARGO_HOME\" \"$RUSTUP_HOME\" \"$*\" "
        ">>\"$BCODEX_TEST_RUSTUP_BOOTSTRAP_LOG\"\n"
        "[ \"$BCODEX_TEST_RUSTUP_BOOTSTRAP_SUCCESS\" = 1 ] || exit 8\n"
        "mkdir -p \"$CARGO_HOME/bin\" \"$RUSTUP_HOME\"\n"
        "cp \"$BCODEX_TEST_RUSTUP_PROGRAM\" \"$CARGO_HOME/bin/rustup\"\n"
        "chmod 0755 \"$CARGO_HOME/bin/rustup\"\n",
    )
    (fake_bin / "rustup").unlink(missing_ok=True)
    (home / ".cargo" / "bin" / "rustup").unlink(missing_ok=True)
    if rustup_location == "path":
        write_executable(
            fake_bin / "rustup", rustup_fixture.read_text(encoding="utf-8")
        )
    elif rustup_location == "home":
        hidden_rustup = home / ".cargo" / "bin" / "rustup"
        hidden_rustup.parent.mkdir(parents=True, exist_ok=True)
        write_executable(hidden_rustup, rustup_fixture.read_text(encoding="utf-8"))

    environment = os.environ.copy()
    for name in (
        "BCODEX_INSTALL_DIR",
        "BCODEX_INSTALL_REVISION",
        "BCODEX_REPOSITORY",
        "BCODEX_BUILD_INPUT_HASH",
        "BCODEX_SOURCE_REVISION",
        "CARGO_BUILD_JOBS",
        "CARGO_BUILD_BUILD_DIR",
        "CARGO_BUILD_TARGET",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_HOME",
        "CARGO_INCREMENTAL",
        "CARGO_INSTALL_ROOT",
        "CARGO_TARGET_DIR",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTC_WRAPPER",
        "RUSTFLAGS",
        "RUSTUP_HOME",
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
            "BCODEX_TEST_ADVANCING_MAIN": "1" if advancing_main else "0",
            "BCODEX_TEST_ARCHIVE_FAILURES": str(archive_failures),
            "BCODEX_TEST_ARCHIVE_REQUEST_COUNT_FILE": str(
                root / "archive-request-count"
            ),
            "BCODEX_TEST_BUILD_LOG": str(root / "build.log"),
            "BCODEX_TEST_BUILD_SUCCESS": "1" if build_success else "0",
            "BCODEX_TEST_BUILD_TOOLS_INSTALL_SUCCESS": (
                "1" if build_tools_install_success else "0"
            ),
            "BCODEX_TEST_BUILT_VERSION": VERSION,
            "BCODEX_TEST_COMMIT": COMMIT,
            "BCODEX_TEST_COMPILE_LOG": str(root / "compile.log"),
            "BCODEX_TEST_CODESIGN_LOG": str(root / "codesign.log"),
            "BCODEX_TEST_CODESIGN_SUCCESS": "1" if codesign_success else "0",
            "BCODEX_TEST_CLEANUP_SUCCESS": "1" if cleanup_success else "0",
            "BCODEX_TEST_COMPILER_INSTALLED_FILE": str(root / "compiler-installed"),
            "BCODEX_TEST_COMPILER_WORKS": "1" if compiler_works else "0",
            "BCODEX_TEST_DOWNLOAD_LOG": str(root / "download.log"),
            "BCODEX_TEST_EMBEDDED_BUILD_INPUT_HASH": embedded_build_input_hash or "",
            "BCODEX_TEST_EMBEDDED_REVISION": embedded_revision or "",
            "BCODEX_TEST_FAKE_BIN": str(fake_bin),
            "BCODEX_TEST_GITHUB_LOG": str(root / "github.log"),
            "BCODEX_TEST_MAIN_FAILURES": str(main_failures),
            "BCODEX_TEST_MAIN_REQUEST_COUNT_FILE": str(root / "main-request-count"),
            "BCODEX_TEST_NEXT_COMMIT": next_commit,
            "BCODEX_TEST_REPOSITORY": "ummay0432/bettercodex",
            "BCODEX_TEST_RUSTUP_BOOTSTRAP_LOG": str(root / "rustup-bootstrap.log"),
            "BCODEX_TEST_RUSTUP_BOOTSTRAP_SUCCESS": (
                "1" if rustup_bootstrap_success else "0"
            ),
            "BCODEX_TEST_RUSTUP_INSTALLER": str(root / "rustup-init-fixture"),
            "BCODEX_TEST_RUSTUP_LOG": str(root / "rustup.log"),
            "BCODEX_TEST_RUSTUP_PROGRAM": str(rustup_fixture),
            "BCODEX_TEST_SMOKE_SUCCESS": "1" if smoke_success else "0",
            "BCODEX_TEST_SOURCE_ARCHIVE": str(archive),
            "BCODEX_TEST_SUDO_LOG": str(root / "sudo.log"),
            "BCODEX_TEST_SYSTEM_PACKAGES_LOG": str(root / "system-packages.log"),
            "BCODEX_TEST_TOOLCHAIN_INSTALLED": "1" if toolchain_installed else "0",
            "BCODEX_TEST_XCODE_SELECT_LOG": str(root / "xcode-select.log"),
            "PATH": f"{fake_bin}:{system_bin}",
            "SHELL": shell,
            "TMPDIR": str(temporary),
        }
    )
    if xdg_cache_enabled:
        environment["XDG_CACHE_HOME"] = str(root / "cache")
    if install_revision is not None:
        environment["BCODEX_INSTALL_REVISION"] = install_revision
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


def create_source_archive(root: Path, source_generation: str) -> Path:
    archive = root / "source.tar.gz"
    prefix = f"ummay0432-bettercodex-{COMMIT[:7]}"
    files = {
        "Cargo.toml": (
            f'[package]\nname = "bettercodex"\nversion = "{VERSION}"\n'
            f"# source generation {source_generation}\n"
        ),
        "Cargo.lock": f"# fixture lockfile {source_generation}\n",
        "rust-toolchain.toml": '[toolchain]\nchannel = "1.95.0"\n',
        "src/main.rs": "fn main() {}\n",
        "scripts/cargo-with-v8.sh": textwrap.dedent(
            """\
            #!/bin/sh
            cargo_download="$CARGO_HOME/git/dependency-download"
            if [ ! -f "$cargo_download" ]; then
              mkdir -p "$(dirname "$cargo_download")"
              printf '%s\\n' cargo >>"$BCODEX_TEST_DOWNLOAD_LOG"
              : >"$cargo_download"
            fi
            v8_download="$XDG_CACHE_HOME/bettercodex/rusty-v8-fixture/artifact"
            if [ ! -f "$v8_download" ]; then
              mkdir -p "$(dirname "$v8_download")"
              printf '%s\\n' v8 >>"$BCODEX_TEST_DOWNLOAD_LOG"
              : >"$v8_download"
            fi
            compiled_dependency="$CARGO_TARGET_DIR/release/deps/fixture-dependency.rlib"
            if [ ! -f "$compiled_dependency" ]; then
              mkdir -p "$(dirname "$compiled_dependency")"
              printf '%s\\n' dependency >>"$BCODEX_TEST_COMPILE_LOG"
              : >"$compiled_dependency"
            fi
            printf '%s|%s|%s|%s|%s|%s|%s\\n' \
              "$CARGO_INCREMENTAL" "$CARGO_HOME" "$CARGO_TARGET_DIR" \
              "$XDG_CACHE_HOME" "$TMPDIR" "$BCODEX_BUILD_INPUT_HASH" \
              "$*" >>"$BCODEX_TEST_BUILD_LOG"
            [ "$BCODEX_TEST_BUILD_SUCCESS" = "1" ] || exit 9
            mkdir -p "$CARGO_TARGET_DIR/release"
            {
              printf '%s\\n' '#!/bin/sh'
              printf 'version=%s\\n' "$BCODEX_TEST_BUILT_VERSION"
              printf '%s\\n' 'source_revision=unpatched'
              printf 'build_input_hash=%s\\n' \
                "${BCODEX_TEST_EMBEDDED_BUILD_INPUT_HASH:-$BCODEX_BUILD_INPUT_HASH}"
              printf 'smoke_success=%s\\n' "$BCODEX_TEST_SMOKE_SUCCESS"
              printf '%s\\n' 'case "${1:-}" in'
              printf '%s\\n' '  --version) printf "bcodex %s\\n" "$version" ;;'
              printf '%s\\n' '  --internal-source-revision) printf "%s\\n" "$source_revision" ;;'
              printf '%s\\n' '  --internal-install-stage)'
              printf '%s\\n' '    [ "$#" -eq 4 ] || exit 14'
              printf '%s\\n' '    [ "$build_input_hash" = "$4" ] || exit 15'
              printf '%s\\n' '    staged_revision="${BCODEX_TEST_EMBEDDED_REVISION:-$3}"'
              printf '%s\\n' '    sed "s/^source_revision=.*/source_revision=$staged_revision/" "$0" >"$2"'
              printf '%s\\n' '    ;;'
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
