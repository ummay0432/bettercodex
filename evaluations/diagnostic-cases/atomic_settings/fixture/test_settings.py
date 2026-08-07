import unittest

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
