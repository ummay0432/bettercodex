#!/usr/bin/env python3
"""Tests for bettercodex's dependency-free developer workflows."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

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


if __name__ == "__main__":
    unittest.main()
