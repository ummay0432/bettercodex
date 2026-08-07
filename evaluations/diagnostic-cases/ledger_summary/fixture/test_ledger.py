import unittest

from ledger import summarize


class LedgerTests(unittest.TestCase):
    def test_refunds_and_account_order(self) -> None:
        self.assertEqual(
            summarize(["zeta,charge,2.00", "alpha,charge,3", "zeta,refund,0.25"]),
            "alpha=300\nzeta=175",
        )

    def test_blank_lines_are_ignored(self) -> None:
        self.assertEqual(summarize(["  \t", "main,charge,0.01"]), "main=1")


if __name__ == "__main__":
    unittest.main()
