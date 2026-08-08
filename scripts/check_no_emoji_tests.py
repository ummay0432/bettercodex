#!/usr/bin/env python3
"""Tests for the repository emoji policy check."""

from __future__ import annotations

from pathlib import Path
import subprocess
import tempfile
import unittest


CHECK_SCRIPT = Path(__file__).with_name("check-no-emoji.sh")


class CheckNoEmojiTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="bettercodex-emoji-test-")
        self.repository = Path(self.temporary.name)
        subprocess.run(
            ["git", "init", "--quiet", "--initial-branch=main"],
            cwd=self.repository,
            check=True,
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def run_check(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [CHECK_SCRIPT],
            cwd=self.repository,
            capture_output=True,
            text=True,
        )

    def test_accepts_non_emoji_unicode_and_the_bare_checkmark(self) -> None:
        text = "caf" + chr(0x00E9) + "\n" + chr(0x2714) + "\n"
        (self.repository / "clean.txt").write_text(text, encoding="utf-8")

        result = self.run_check()

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_emoji_in_untracked_and_tracked_text(self) -> None:
        emoji = chr(0x1F600)
        untracked = self.repository / "untracked.txt"
        untracked.write_text(f"bad {emoji}\n", encoding="utf-8")

        first = self.run_check()
        self.assertEqual(first.returncode, 1)
        self.assertIn("untracked.txt:1", first.stdout)

        subprocess.run(["git", "add", "untracked.txt"], cwd=self.repository, check=True)
        second = self.run_check()
        self.assertEqual(second.returncode, 1)
        self.assertIn("untracked.txt:1", second.stdout)

    def test_rejects_emoji_presentation_for_the_allowed_checkmark(self) -> None:
        checkmark_emoji = chr(0x2714) + chr(0xFE0F)
        (self.repository / "checkmark.txt").write_text(
            checkmark_emoji + "\n", encoding="utf-8"
        )

        result = self.run_check()

        self.assertEqual(result.returncode, 1)
        self.assertIn("checkmark.txt:1", result.stdout)

    def test_rejects_emoji_in_a_path(self) -> None:
        path = self.repository / f"bad-{chr(0x1F600)}.txt"
        path.write_text("plain text\n", encoding="utf-8")

        result = self.run_check()

        self.assertEqual(result.returncode, 1)
        self.assertIn("disallowed emoji in path", result.stdout)

    def test_skips_ignored_files_and_binary_contents(self) -> None:
        emoji = chr(0x1F600)
        (self.repository / ".gitignore").write_text("ignored.txt\n", encoding="utf-8")
        (self.repository / "ignored.txt").write_text(emoji + "\n", encoding="utf-8")
        (self.repository / "binary.dat").write_bytes(b"\0" + emoji.encode("utf-8"))

        result = self.run_check()

        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
