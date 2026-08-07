#!/usr/bin/env python3
"""Tests for bettercodex's dependency-free developer workflows."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import dev


class GitRepository:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="bettercodex-dev-tests-")
        self.path = Path(self.temporary.name)
        self.run("init", "--initial-branch=main")
        self.run("config", "user.email", "bettercodex-tests@example.invalid")
        self.run("config", "user.name", "bettercodex tests")
        (self.path / "tracked.txt").write_text("first\n")
        self.run("add", "tracked.txt")
        self.run("commit", "-m", "first")

    def run(self, *arguments: str) -> str:
        result = subprocess.run(
            ["git", *arguments],
            cwd=self.path,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        return result.stdout.strip()

    def commit(self, text: str) -> str:
        (self.path / "tracked.txt").write_text(f"{text}\n")
        self.run("add", "tracked.txt")
        self.run("commit", "-m", text)
        return self.run("rev-parse", "HEAD")

    def close(self) -> None:
        self.temporary.cleanup()


class CanonicalInstallTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repository = GitRepository()

    def tearDown(self) -> None:
        self.repository.close()

    def test_canonical_main_allows_local_commits_ahead_of_origin(self) -> None:
        original = self.repository.run("rev-parse", "HEAD")
        self.repository.run("update-ref", dev.CANONICAL_REMOTE_BRANCH, original)
        latest = self.repository.commit("second")

        self.assertEqual(dev.canonical_main_commit(self.repository.path), latest)

    def test_canonical_main_rejects_origin_ahead_of_local_main(self) -> None:
        original = self.repository.run("rev-parse", "HEAD")
        remote = self.repository.commit("remote")
        self.repository.run("switch", "--detach", original)
        self.repository.run("branch", "--force", "main", original)
        self.repository.run("update-ref", dev.CANONICAL_REMOTE_BRANCH, remote)

        with self.assertRaisesRegex(RuntimeError, "does not contain origin/main"):
            dev.canonical_main_commit(self.repository.path)

    def test_install_caller_must_be_clean_and_integrated(self) -> None:
        main = self.repository.run("rev-parse", "HEAD")
        self.repository.run("switch", "-c", "feature")
        self.repository.commit("feature")

        with self.assertRaisesRegex(RuntimeError, "not integrated"):
            dev.validate_install_caller(self.repository.path, main)

        self.repository.run("switch", "main")
        (self.repository.path / "untracked.txt").write_text("dirty\n")
        with self.assertRaisesRegex(RuntimeError, "dirty"):
            dev.validate_install_caller(self.repository.path, main)

    def test_committed_snapshot_excludes_worktree_changes(self) -> None:
        commit = self.repository.run("rev-parse", "HEAD")
        (self.repository.path / "tracked.txt").write_text("dirty\n")
        (self.repository.path / "untracked.txt").write_text("untracked\n")

        with dev.committed_source_snapshot(self.repository.path, commit) as source:
            self.assertEqual((source / "tracked.txt").read_text(), "first\n")
            self.assertFalse((source / "untracked.txt").exists())


class SandboxedV8Tests(unittest.TestCase):
    def test_pinned_artifact_version_matches_cargo_lock(self) -> None:
        self.assertEqual(dev.locked_package_version("v8"), dev.V8_VERSION)

    def test_locked_package_version_rejects_ambiguous_versions(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bettercodex-lock-test-") as temporary:
            lockfile = Path(temporary) / "Cargo.lock"
            lockfile.write_text(
                '[[package]]\nname = "v8"\nversion = "1.0.0"\n\n'
                '[[package]]\nname = "v8"\nversion = "2.0.0"\n'
            )

            with self.assertRaisesRegex(RuntimeError, "exactly one v8 version"):
                dev.locked_package_version("v8", lockfile)

    def test_partial_artifact_override_is_rejected(self) -> None:
        environment = {"RUSTY_V8_ARCHIVE": "/tmp/archive"}

        with self.assertRaisesRegex(RuntimeError, "must be set together"):
            dev.configure_v8_environment(environment, target="unused")

    def test_complete_override_and_source_build_are_preserved(self) -> None:
        overridden = {
            "RUSTY_V8_ARCHIVE": "/tmp/archive",
            "RUSTY_V8_SRC_BINDING_PATH": "/tmp/binding",
        }
        dev.configure_v8_environment(overridden, target="unused")
        self.assertEqual(overridden["RUSTY_V8_ARCHIVE"], "/tmp/archive")

        source_build = {"V8_FROM_SOURCE": "true"}
        dev.configure_v8_environment(source_build, target="unused")
        self.assertNotIn("RUSTY_V8_ARCHIVE", source_build)

    def test_downloads_and_verifies_the_pinned_artifact_pair(self) -> None:
        target = "test-target"
        archive_name, binding_name = dev.v8_artifact_names(target)
        with tempfile.TemporaryDirectory(prefix="bettercodex-v8-test-") as temporary:
            root = Path(temporary)
            source = root / "source"
            cache = root / "cache"
            source.mkdir()
            archive_bytes = b"sandboxed archive"
            binding_bytes = b"matching bindings"
            (source / archive_name).write_bytes(archive_bytes)
            (source / binding_name).write_bytes(binding_bytes)
            checksums = {
                target: (
                    dev.hashlib.sha256(archive_bytes).hexdigest(),
                    dev.hashlib.sha256(binding_bytes).hexdigest(),
                )
            }
            environment: dict[str, str] = {}

            with (
                mock.patch.object(dev, "V8_CHECKSUMS", checksums),
                mock.patch.object(dev, "V8_RELEASE", source.as_uri()),
            ):
                dev.configure_v8_environment(
                    environment,
                    target=target,
                    cache_root=cache,
                )

            archive = Path(environment["RUSTY_V8_ARCHIVE"])
            binding = Path(environment["RUSTY_V8_SRC_BINDING_PATH"])
            self.assertEqual(archive.read_bytes(), archive_bytes)
            self.assertEqual(binding.read_bytes(), binding_bytes)

    def test_rejects_an_artifact_with_the_wrong_checksum(self) -> None:
        target = "test-target"
        archive_name, binding_name = dev.v8_artifact_names(target)
        with tempfile.TemporaryDirectory(prefix="bettercodex-v8-test-") as temporary:
            root = Path(temporary)
            source = root / "source"
            cache = root / "cache"
            source.mkdir()
            (source / archive_name).write_bytes(b"unexpected archive")
            (source / binding_name).write_bytes(b"matching bindings")
            checksums = {target: ("0" * 64, dev.hashlib.sha256(b"matching bindings").hexdigest())}

            with (
                mock.patch.object(dev, "V8_CHECKSUMS", checksums),
                mock.patch.object(dev, "V8_RELEASE", source.as_uri()),
                self.assertRaisesRegex(RuntimeError, "expected 000000"),
            ):
                dev.configure_v8_environment(
                    {},
                    target=target,
                    cache_root=cache,
                )

            self.assertFalse((cache / archive_name).exists())


if __name__ == "__main__":
    unittest.main()
