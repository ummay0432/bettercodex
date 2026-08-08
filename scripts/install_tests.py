#!/usr/bin/env python3

from __future__ import annotations

import io
import gzip
import hashlib
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

    def test_install_and_update_reuse_downloads_and_compiled_dependencies(self) -> None:
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
            self.assertIn("retained compiled dependencies", second.stdout)
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
            for build in builds:
                revision, cargo_home, target, cache_home, compiler_tmp, arguments = build.split(
                    "|", 5
                )
                self.assertEqual(revision, COMMIT)
                self.assertEqual(arguments, "build --release --locked --bin bcodex")
                self.assertEqual(Path(cargo_home), expected_cargo_home)
                self.assertEqual(Path(cache_home), expected_cache_home)
                self.assertEqual(Path(target), expected_target)
                self.assertIn("bettercodex-install.", compiler_tmp)
                self.assertFalse(Path(compiler_tmp).exists())
            self.assertTrue(expected_cargo_home.is_dir())
            self.assertTrue((expected_target / "release" / "deps" / "fixture-dependency.rlib").is_file())
            self.assertFalse((expected_target / "release" / "bcodex").exists())
            self.assertEqual(
                (root / "compile.log").read_text(encoding="utf-8").splitlines(),
                ["dependency"],
            )
            self.assertEqual(
                (root / "download.log").read_text(encoding="utf-8").splitlines(),
                ["cargo", "v8"],
            )
            assert_no_installer_residue(self, root)

    def test_published_native_release_avoids_every_source_build_tool(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            updater_cache = root / "cache" / "bettercodex"
            (updater_cache / "build" / "old-target").mkdir(parents=True)
            (updater_cache / "cargo" / "registry").mkdir(parents=True)
            (updater_cache / "tmp" / "source").mkdir(parents=True)
            (updater_cache / "rusty-v8-development-cache").mkdir(parents=True)
            (updater_cache / "keep").write_text("operator data", encoding="utf-8")

            result = run_installer(root, prebuilt=True)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Downloading the native BetterCodex executable", result.stdout)
            self.assertIn("without Rust, Cargo, or local compilation", result.stdout)
            self.assertNotIn("Compiling BetterCodex", result.stdout)
            installed = root / "install" / "bin" / "bcodex"
            self.assertEqual(run_binary(installed, "--version"), f"bcodex {VERSION}\n")
            self.assertEqual(
                run_binary(installed, "--internal-source-revision"), f"{COMMIT}\n"
            )
            self.assertFalse((root / "build.log").exists())
            self.assertFalse((root / "rustup.log").exists())
            self.assertFalse((updater_cache / "build").exists())
            self.assertFalse((updater_cache / "cargo").exists())
            self.assertFalse((updater_cache / "tmp").exists())
            self.assertTrue((updater_cache / "rusty-v8-development-cache").is_dir())
            self.assertEqual(
                (updater_cache / "keep").read_text(encoding="utf-8"),
                "operator data",
            )
            requests = (root / "github.log").read_text(encoding="utf-8")
            target = "x86_64-unknown-linux-gnu"
            release_tag = f"bcodex-v{VERSION}-{COMMIT}"
            self.assertIn(f"releases/download/{release_tag}/bcodex-{target}.gz.sha256", requests)
            self.assertIn(f"releases/download/{release_tag}/bcodex-{target}.gz", requests)
            self.assertEqual(
                (root / "release-request-count").read_text(encoding="utf-8").strip(),
                "2",
            )
            assert_no_installer_residue(self, root)

    def test_compact_release_metadata_selects_the_prebuilt_asset(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(
                root,
                prebuilt=True,
                compact_release_metadata=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("without Rust, Cargo, or local compilation", result.stdout)
            self.assertFalse((root / "build.log").exists())
            installed = root / "install" / "bin" / "bcodex"
            self.assertEqual(run_binary(installed, "--version"), f"bcodex {VERSION}\n")
            assert_no_installer_residue(self, root)

    def test_prebuilt_cleanup_never_follows_a_symlinked_cache_root(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            outside = root / "outside-cache"
            (outside / "build").mkdir(parents=True)
            keep = outside / "build" / "operator-data"
            keep.write_text("keep\n", encoding="utf-8")
            cache_base = root / "cache"
            cache_base.mkdir()
            (cache_base / "bettercodex").symlink_to(outside, target_is_directory=True)

            result = run_installer(root, prebuilt=True)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("not removing symlinked source-updater cache root", result.stderr)
            self.assertEqual(keep.read_text(encoding="utf-8"), "keep\n")
            self.assertTrue((cache_base / "bettercodex").is_symlink())
            assert_no_installer_residue(self, root)

    def test_explicit_release_avoids_the_duplicate_initial_metadata_lookup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(
                root,
                prebuilt=True,
                release_via_environment=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                (root / "release-request-count").read_text(encoding="utf-8").strip(),
                "1",
            )
            self.assertFalse((root / "build.log").exists())
            assert_no_installer_residue(self, root)

    def test_release_lookup_failure_never_turns_into_an_unexpected_source_build(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(root, prebuilt=True, release_failures=3)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("could not resolve the latest published", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
            self.assertFalse((root / "build.log").exists())
            assert_no_installer_residue(self, root)

    def test_confirmed_missing_native_asset_uses_the_bounded_source_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(
                root,
                prebuilt=True,
                prebuilt_asset_available=False,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("no compatible prebuilt asset", result.stderr)
            self.assertIn("falling back to a local source build", result.stderr)
            self.assertIn("Compiling BetterCodex", result.stdout)
            self.assertEqual(
                run_binary(root / "install" / "bin" / "bcodex", "--internal-source-revision"),
                f"{COMMIT}\n",
            )
            self.assertTrue((root / "build.log").is_file())
            assert_no_installer_residue(self, root)

    def test_wrong_release_revision_preserves_the_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(
                root,
                prebuilt=True,
                embedded_revision=NEXT_COMMIT,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("did not embed source revision", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
            self.assertFalse((root / "build.log").exists())
            assert_no_installer_residue(self, root)

    def test_corrupt_or_mismatched_release_asset_never_replaces_or_builds(self) -> None:
        cases = (
            ({"prebuilt_checksum_valid": False}, "has SHA-256"),
            ({"prebuilt_corrupt": True}, "could not be decompressed"),
        )
        for options, expected_error in cases:
            with self.subTest(expected_error=expected_error), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                installed = existing_binary(root)

                result = run_installer(root, prebuilt=True, **options)

                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected_error, result.stderr)
                self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
                self.assertFalse((root / "build.log").exists())
                assert_no_installer_residue(self, root)

    def test_symlinked_destination_is_refused_before_network_or_build_work(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            install_dir = root / "install" / "bin"
            install_dir.mkdir(parents=True)
            outside = root / "outside-binary"
            outside.write_text("operator data\n", encoding="utf-8")
            (install_dir / "bcodex").symlink_to(outside)

            result = run_installer(root, prebuilt=True)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("refusing to replace symlinked", result.stderr)
            self.assertEqual(outside.read_text(encoding="utf-8"), "operator data\n")
            self.assertFalse((root / "github.log").exists())
            self.assertFalse((root / "build.log").exists())

    def test_retries_with_the_same_dependency_cache_when_main_advances(self) -> None:
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
            self.assertEqual(builds[0].split("|")[2], builds[1].split("|")[2])
            self.assertEqual(
                (root / "compile.log").read_text(encoding="utf-8").splitlines(),
                ["dependency"],
            )
            requests = (root / "github.log").read_text(encoding="utf-8")
            self.assertIn(f"tar.gz/{COMMIT}", requests)
            self.assertIn(f"tar.gz/{NEXT_COMMIT}", requests)
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
                assert_no_installer_residue(self, root)

    def test_all_supported_native_hosts_select_their_prebuilt_asset(self) -> None:
        platforms = (
            ("Darwin", "arm64", "aarch64-apple-darwin"),
            ("Darwin", "x86_64", "x86_64-apple-darwin"),
            ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
            ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
        )
        for system, machine, host_target in platforms:
            with self.subTest(host_target=host_target), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)

                result = run_installer(root, system=system, machine=machine, prebuilt=True)

                self.assertEqual(result.returncode, 0, result.stderr)
                requests = (root / "github.log").read_text(encoding="utf-8")
                self.assertIn(f"bcodex-{host_target}.gz.sha256", requests)
                self.assertIn(f"bcodex-{host_target}.gz", requests)
                self.assertFalse((root / "build.log").exists())
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

    def test_missing_pinned_rust_toolchain_is_installed_only_in_the_temp_tree(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, toolchain_installed=False)

            self.assertEqual(result.returncode, 0, result.stderr)
            rustup_calls = (root / "rustup.log").read_text(encoding="utf-8").splitlines()
            install_home, install_arguments = next(
                call.split("|", 1)
                for call in rustup_calls
                if "toolchain install" in call
            )
            self.assertEqual(
                install_arguments,
                "toolchain install 1.95.0 --profile minimal",
            )
            self.assertIn("bettercodex-install.", install_home)
            self.assertTrue(install_home.endswith("/rustup-home"))
            self.assertFalse(Path(install_home).exists())
            self.assertIn("retained compiled dependencies", result.stdout)
            assert_no_installer_residue(self, root)

    def test_known_revision_skips_the_redundant_initial_main_lookup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, install_revision=COMMIT)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                (root / "main-request-count").read_text(encoding="utf-8").strip(),
                "1",
            )
            self.assertEqual(
                run_binary(root / "install" / "bin" / "bcodex", "--internal-source-revision"),
                f"{COMMIT}\n",
            )
            assert_no_installer_residue(self, root)

    def test_changed_build_identity_replaces_one_cache_generation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            first = run_installer(root)
            identity = (
                root
                / "cache"
                / "bettercodex"
                / "build"
                / "x86_64-unknown-linux-gnu"
                / "identity"
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            identity.write_text("incompatible fixture\n", encoding="utf-8")

            second = run_installer(root)

            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertIn("Resetting incompatible compiled-dependency cache", second.stdout)
            self.assertEqual(
                (root / "compile.log").read_text(encoding="utf-8").splitlines(),
                ["dependency", "dependency"],
            )
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
                "4",
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
            self.assertIn("could not resolve the current BetterCodex main commit", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
            self.assertFalse((root / "build.log").exists())
            assert_no_installer_residue(self, root)

    def test_fresh_macos_without_command_line_tools_gets_the_exact_remedy(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            result = run_installer(root, system="Darwin", machine="arm64", compiler_works=False)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("xcode-select --install", result.stderr)
            self.assertFalse((root / "build.log").exists())
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
            outside.mkdir()
            keep = outside / "keep"
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

    def test_continuously_advancing_main_stops_after_three_attempts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = existing_binary(root)

            result = run_installer(root, advancing_main=True, next_commit=NEXT_COMMIT)

            self.assertNotEqual(
                result.returncode,
                0,
                f"{result.stdout}\n{result.stderr}\n{(root / 'github.log').read_text(encoding='utf-8')}",
            )
            self.assertIn("main kept advancing during all 3 build attempts", result.stderr)
            self.assertEqual(installed.read_text(encoding="utf-8"), "existing binary\n")
            builds = (root / "build.log").read_text(encoding="utf-8").splitlines()
            self.assertEqual(
                [build.split("|", 1)[0] for build in builds],
                [COMMIT, NEXT_COMMIT, "c" * 40],
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
            self.assertIn("another BetterCodex install is already running", result.stderr)
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
    compiler_works: bool = True,
    embedded_revision: str | None = None,
    install_revision: str | None = None,
    prebuilt: bool = False,
    prebuilt_asset_available: bool = True,
    prebuilt_checksum_valid: bool = True,
    prebuilt_corrupt: bool = False,
    release_via_environment: bool = False,
    release_failures: int = 0,
    compact_release_metadata: bool = False,
    main_failures: int = 0,
    smoke_success: bool = True,
    next_commit: str = COMMIT,
    home_enabled: bool = True,
    toolchain_installed: bool = True,
    shell: str = "/bin/sh",
    cleanup_success: bool = True,
    advancing_main: bool = False,
    shadowed_binary: bool = False,
    xdg_cache_enabled: bool = True,
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
    prebuilt_asset, prebuilt_sha256 = create_prebuilt_asset(
        root,
        revision=embedded_revision or COMMIT,
        smoke_success=smoke_success,
    )
    if prebuilt_corrupt:
        prebuilt_asset.write_bytes(b"not a gzip stream")
        prebuilt_sha256 = hashlib.sha256(prebuilt_asset.read_bytes()).hexdigest()
    if not prebuilt_checksum_valid:
        prebuilt_sha256 = "0" * 64

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
              "https://api.github.com/repos/$BCODEX_TEST_REPOSITORY/releases/latest")
                count=0
                if [ -f "$BCODEX_TEST_RELEASE_REQUEST_COUNT_FILE" ]; then
                  count="$(sed -n '1p' "$BCODEX_TEST_RELEASE_REQUEST_COUNT_FILE")"
                fi
                count=$((count + 1))
                printf '%s\\n' "$count" >"$BCODEX_TEST_RELEASE_REQUEST_COUNT_FILE"
                if [ "$count" -le "$BCODEX_TEST_RELEASE_FAILURES" ]; then
                  emit_empty 503
                  exit 0
                fi
                if [ "$BCODEX_TEST_PREBUILT" != 1 ]; then
                  emit_empty 404
                  exit 0
                fi
                if [ "$BCODEX_TEST_COMPACT_RELEASE_METADATA" = 1 ]; then
                  if [ -n "$output" ]; then
                    printf '{"tag_name":"%s","draft":false,"prerelease":false}\\n' \
                      "$BCODEX_TEST_RELEASE_TAG" >"$output"
                  else
                    printf '{"tag_name":"%s","draft":false,"prerelease":false}\\n' \
                      "$BCODEX_TEST_RELEASE_TAG"
                  fi
                elif [ -n "$output" ]; then
                  printf '{\\n  "tag_name": "%s",\\n  "draft": false,\\n  "prerelease": false\\n}\\n' \
                    "$BCODEX_TEST_RELEASE_TAG" >"$output"
                else
                  printf '{\\n  "tag_name": "%s",\\n  "draft": false,\\n  "prerelease": false\\n}\\n' \
                    "$BCODEX_TEST_RELEASE_TAG"
                fi
                emit_status 200
                ;;
              "https://github.com/$BCODEX_TEST_REPOSITORY/releases/download/"*/*.sha256)
                if [ "$BCODEX_TEST_PREBUILT_ASSET_AVAILABLE" != 1 ]; then
                  emit_empty 404
                  exit 0
                fi
                asset_name="${url##*/}"
                asset_name="${asset_name%.sha256}"
                if [ -n "$output" ]; then
                  printf '%s  %s\\n' "$BCODEX_TEST_PREBUILT_SHA256" "$asset_name" >"$output"
                else
                  printf '%s  %s\\n' "$BCODEX_TEST_PREBUILT_SHA256" "$asset_name"
                fi
                emit_status 200
                ;;
              "https://github.com/$BCODEX_TEST_REPOSITORY/releases/download/"*/*.gz)
                if [ "$BCODEX_TEST_PREBUILT_ASSET_AVAILABLE" != 1 ]; then
                  emit_empty 404
                  exit 0
                fi
                emit_file "$BCODEX_TEST_PREBUILT_ASSET" 200
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
    write_executable(
        fake_bin / "cc",
        "#!/bin/sh\n[ \"$BCODEX_TEST_COMPILER_WORKS\" = 1 ]\n",
    )
    write_executable(fake_bin / "sleep", "#!/bin/sh\nexit 0\n")
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
    write_executable(
        fake_bin / "rustup",
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

    environment = os.environ.copy()
    for name in (
        "BCODEX_INSTALL_DIR",
        "BCODEX_INSTALL_RELEASE_TAG",
        "BCODEX_INSTALL_REVISION",
        "BCODEX_INSTALL_VERSION",
        "BCODEX_REPOSITORY",
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
            "BCODEX_TEST_BUILT_VERSION": VERSION,
            "BCODEX_TEST_COMMIT": COMMIT,
            "BCODEX_TEST_COMPILE_LOG": str(root / "compile.log"),
            "BCODEX_TEST_CLEANUP_SUCCESS": "1" if cleanup_success else "0",
            "BCODEX_TEST_COMPILER_WORKS": "1" if compiler_works else "0",
            "BCODEX_TEST_COMPACT_RELEASE_METADATA": (
                "1" if compact_release_metadata else "0"
            ),
            "BCODEX_TEST_DOWNLOAD_LOG": str(root / "download.log"),
            "BCODEX_TEST_EMBEDDED_REVISION": embedded_revision or "",
            "BCODEX_TEST_FAKE_BIN": str(fake_bin),
            "BCODEX_TEST_GITHUB_LOG": str(root / "github.log"),
            "BCODEX_TEST_MAIN_FAILURES": str(main_failures),
            "BCODEX_TEST_MAIN_REQUEST_COUNT_FILE": str(root / "main-request-count"),
            "BCODEX_TEST_NEXT_COMMIT": next_commit,
            "BCODEX_TEST_PREBUILT": "1" if prebuilt else "0",
            "BCODEX_TEST_PREBUILT_ASSET": str(prebuilt_asset),
            "BCODEX_TEST_PREBUILT_ASSET_AVAILABLE": (
                "1" if prebuilt_asset_available else "0"
            ),
            "BCODEX_TEST_PREBUILT_SHA256": prebuilt_sha256,
            "BCODEX_TEST_RELEASE_FAILURES": str(release_failures),
            "BCODEX_TEST_RELEASE_REQUEST_COUNT_FILE": str(
                root / "release-request-count"
            ),
            "BCODEX_TEST_RELEASE_TAG": f"bcodex-v{VERSION}-{COMMIT}",
            "BCODEX_TEST_REPOSITORY": "ummay0432/bettercodex",
            "BCODEX_TEST_RUSTUP_LOG": str(root / "rustup.log"),
            "BCODEX_TEST_SMOKE_SUCCESS": "1" if smoke_success else "0",
            "BCODEX_TEST_SOURCE_ARCHIVE": str(archive),
            "BCODEX_TEST_TOOLCHAIN_INSTALLED": "1" if toolchain_installed else "0",
            "PATH": f"{fake_bin}:/usr/bin:/bin",
            "SHELL": shell,
            "TMPDIR": str(temporary),
        }
    )
    if xdg_cache_enabled:
        environment["XDG_CACHE_HOME"] = str(root / "cache")
    if install_revision is not None:
        environment["BCODEX_INSTALL_REVISION"] = install_revision
    if release_via_environment:
        environment["BCODEX_INSTALL_RELEASE_TAG"] = f"bcodex-v{VERSION}-{COMMIT}"
        environment["BCODEX_INSTALL_REVISION"] = COMMIT
        environment["BCODEX_INSTALL_VERSION"] = VERSION
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


def create_prebuilt_asset(
    root: Path,
    *,
    revision: str,
    smoke_success: bool,
) -> tuple[Path, str]:
    binary = textwrap.dedent(
        f"""\
        #!/bin/sh
        version='{VERSION}'
        source_revision='{revision}'
        smoke_success={'1' if smoke_success else '0'}
        case "${{1:-}}" in
          --version) printf 'bcodex %s\\n' "$version" ;;
          --internal-source-revision) printf '%s\\n' "$source_revision" ;;
          --internal-install-smoke)
            [ "$smoke_success" = 1 ] || exit 12
            printf 'bcodex %s install smoke passed\\n' "$version"
            ;;
          *) exit 13 ;;
        esac
        """
    ).encode()
    compressed = gzip.compress(binary, compresslevel=9, mtime=0)
    asset = root / "prebuilt-bcodex.gz"
    asset.write_bytes(compressed)
    return asset, hashlib.sha256(compressed).hexdigest()


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
