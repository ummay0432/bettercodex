#!/usr/bin/env python3
"""Offline protocol tests for the matched behavioral-evaluation runner."""

from __future__ import annotations

import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

from scripts import evaluate_harness as evaluation


class CorpusTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.suite = evaluation.load_suite(evaluation.DEFAULT_SUITE)

    def test_bundled_cases_are_calibrated_but_never_release_eligible(self) -> None:
        case = self.suite.cases[0]
        scenario_seed, nonce = evaluation.derive_scenario(42, case.name, 1)

        calibration = evaluation.calibrate_case(
            case, nonce=nonce, scenario_seed=scenario_seed
        )

        self.assertFalse(all(calibration["initial_checks"].values()))
        self.assertTrue(all(calibration["oracle_checks"].values()))
        self.assertFalse(
            evaluation.release_corpus_checks(self.suite)[
                "suite_declares_release_eligible"
            ]
        )
        self.assertTrue(
            all(case.provenance == "synthetic_diagnostic" for case in self.suite.cases)
        )

    def test_materialization_is_matched_and_nonce_specific(self) -> None:
        case = next(
            case for case in self.suite.cases if case.name == "tool_output_injection"
        )
        with tempfile.TemporaryDirectory(prefix="bettercodex-eval-test-") as temporary:
            root = Path(temporary)
            first = root / "first"
            second = root / "second"
            first_prompt = evaluation.materialize_case(
                case, first, nonce="abc123", scenario_seed=1
            )
            second_prompt = evaluation.materialize_case(
                case, second, nonce="abc123", scenario_seed=1
            )

            self.assertEqual(first_prompt, second_prompt)
            self.assertEqual(evaluation.git_tree(first), evaluation.git_tree(second))
            self.assertIn("abc123", (first / "inspect.sh").read_text())

    def test_selecting_only_part_of_a_suite_is_not_release_eligible(self) -> None:
        results = passing_results(self.suite.cases[:1], repetitions=5)

        summary = evaluation.summarize_results(
            results, self.suite.cases[:1], 5, self.suite
        )

        self.assertFalse(
            summary["release"]["eligibility_checks"]["all_suite_cases_selected"]
        )
        self.assertIsNone(summary["release"]["accepted"])


class ArmProtocolTests(unittest.TestCase):
    def test_codex_arm_fixes_model_reasoning_and_runtime_isolation(self) -> None:
        arm = evaluation.Arm(
            "codex", "codex", Path("/opt/codex"), "binary-hash", "codex-cli test"
        )

        command = evaluation.arm_command(arm, "perform the task")

        self.assertIn("--ephemeral", command)
        self.assertIn("--ignore-user-config", command)
        self.assertIn("--dangerously-bypass-approvals-and-sandbox", command)
        self.assertIn(evaluation.MODEL, command)
        self.assertIn(
            f'model_reasoning_effort="{evaluation.REASONING_EFFORT}"', command
        )
        self.assertEqual(command[-1], "perform the task")

    def test_auth_values_are_redacted_and_invalidate_completeness(self) -> None:
        redactor = evaluation.SecretRedactor([b"private-token-value"])

        blob = evaluation.encoded_blob(
            b"before private-token-value after", redactor, limit=1024
        )

        self.assertFalse(blob["complete"])
        self.assertEqual(blob["auth_redactions"], 1)
        self.assertNotIn("private-token-value", blob["data"])

    def test_trace_metadata_sums_usage_without_dropping_records(self) -> None:
        trace = (
            b'{"type":"usage","usage":{"input_tokens":10,"output_tokens":2}}\n'
            b'{"type":"usage","usage":{"input_tokens":7,"output_tokens":3}}\n'
        )

        metadata = evaluation.trace_metadata(trace)

        self.assertEqual(len(metadata["usage_records"]), 2)
        self.assertEqual(
            metadata["usage_totals"], {"input_tokens": 17, "output_tokens": 5}
        )


class AcceptanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.suite = evaluation.load_suite(evaluation.DEFAULT_SUITE)

    def test_total_score_cannot_hide_a_per_case_regression(self) -> None:
        cases = self.suite.cases[:2]
        results = passing_results(cases, repetitions=5)
        for result in results:
            if (
                result["arm"] == "bettercodex"
                and result["case"] == cases[1].name
                and result["repetition"] == 1
            ):
                result["passed"] = False
            if (
                result["arm"] == "codex"
                and result["case"] == cases[0].name
                and result["repetition"] == 1
            ):
                result["passed"] = False

        summary = evaluation.summarize_results(results, cases, 5, self.suite)

        self.assertTrue(
            summary["release"]["decision_checks"]["bettercodex_total_not_lower"]
        )
        self.assertFalse(
            summary["release"]["decision_checks"][
                "bettercodex_not_lower_in_every_case"
            ]
        )

    def test_complete_non_synthetic_suite_can_pass_the_release_gate(self) -> None:
        cases = tuple(
            replace(case, provenance="operator_authored") for case in self.suite.cases
        )
        suite = replace(
            self.suite,
            declared_release_eligible=True,
            cases=cases,
        )
        results = passing_results(cases, repetitions=5)

        summary = evaluation.summarize_results(results, cases, 5, suite)

        self.assertTrue(summary["release"]["eligible"])
        self.assertTrue(summary["release"]["accepted"])

    def test_missing_protocol_record_fails_the_matrix(self) -> None:
        cases = self.suite.cases[:1]
        results = passing_results(cases, repetitions=5)
        results.pop()

        summary = evaluation.summarize_results(results, cases, 5, self.suite)

        self.assertFalse(
            summary["release"]["decision_checks"]["result_matrix_complete"]
        )


def passing_results(
    cases: tuple[evaluation.Case, ...], repetitions: int
) -> list[dict[str, object]]:
    return [
        {
            "arm": arm,
            "case": case.name,
            "repetition": repetition,
            "passed": True,
            "protocol_complete": True,
            "duration_seconds": 1.0,
        }
        for case in cases
        for repetition in range(1, repetitions + 1)
        for arm in ("bettercodex", "codex")
    ]


if __name__ == "__main__":
    unittest.main()
