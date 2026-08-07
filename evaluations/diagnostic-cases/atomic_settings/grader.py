#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import random
import sys
from pathlib import Path


VISIBLE_TEST = '''import unittest

from settings.store import apply_updates


class SettingsTests(unittest.TestCase):
    def test_returns_copy_and_uses_last_duplicate(self) -> None:
        current = {"workers": 2}
        result = apply_updates(current, ["enabled=true", "workers=3", "workers=4"])
        self.assertEqual(result, {"enabled": True, "workers": 4})
        self.assertEqual(current, {"workers": 2})

    def test_invalid_batch_is_atomic(self) -> None:
        current = {"workers": 2}
        with self.assertRaises(ValueError):
            apply_updates(current, ["workers=5", "bad value=7"])
        self.assertEqual(current, {"workers": 2})


if __name__ == "__main__":
    unittest.main()
'''


def attempt(function, default=False):
    try:
        return bool(function())
    except BaseException:
        return default


root = Path(sys.argv[1])
sys.path.insert(0, str(root))
try:
    from settings.store import apply_updates
except BaseException:
    apply_updates = None


def copy_and_duplicates() -> bool:
    current = {"workers": 2, "stable": True}
    result = apply_updates(current, ["enabled=false", "workers=3", "workers=-7"])
    return result == {"workers": -7, "stable": True, "enabled": False} and current == {
        "workers": 2,
        "stable": True,
    } and result is not current


def invalid_batches_are_atomic() -> bool:
    invalid = ["Bad=1", "bad-key=1", "key=01", "key=+1", "key=1.0", "key= true", "key=1=2"]
    for record in invalid:
        current = {"untouched": 9}
        try:
            apply_updates(current, ["first=1", record, "last=2"])
        except ValueError:
            if current != {"untouched": 9}:
                return False
            continue
        return False
    return True


def randomized_updates() -> bool:
    generator = random.Random(int(os.environ["EVAL_SCENARIO_SEED"]))
    current = {"base": 17}
    expected = dict(current)
    rows = []
    for _ in range(100):
        key = f"key_{generator.randrange(12)}"
        value = generator.randrange(-500_000, 500_001)
        rows.append(f"{key}={value}")
        expected[key] = value
    return apply_updates(current, rows) == expected and current == {"base": 17}


store_source = (root / "settings" / "store.py").read_text(encoding="utf-8")
checks = {
    "module_loaded": apply_updates is not None,
    "copy_and_duplicates": attempt(copy_and_duplicates),
    "invalid_batches_are_atomic": attempt(invalid_batches_are_atomic),
    "randomized_updates": attempt(randomized_updates),
    "reuses_shared_parser": "from .parser import parse_value" in store_source
    and "def _parse_value" not in store_source,
    "visible_tests_unchanged": hashlib.sha256((root / "test_settings.py").read_bytes()).hexdigest()
    == hashlib.sha256(VISIBLE_TEST.encode()).hexdigest(),
}
print(json.dumps(checks, sort_keys=True))
