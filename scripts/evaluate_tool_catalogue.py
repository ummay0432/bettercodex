#!/usr/bin/env python3
"""Run a matched, executable A/B evaluation of BetterCodex tool catalogues.

This is intentionally separate from the deterministic test suite: it makes live
GPT-5.6 Sol requests using the operator's existing Codex authentication. It
copies credentials into a temporary state directory and deletes that directory
when the run finishes.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import re
import shutil
import struct
import subprocess
import tempfile
import time
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Callable


SEED = 20260805
DEFAULT_REPETITIONS = 2
RUN_TIMEOUT_SECONDS = 10 * 60
MAX_RECORDED_OUTPUT_CHARS = 12_000
AGENTS = """# Tool catalogue evaluation fixture

This is an isolated disposable Git repository. Complete the user's requested
task autonomously, use the requested tools when named, and validate mutations.
Do not inspect or modify paths outside this repository.
"""


@dataclass(frozen=True)
class Observation:
    stdout: str
    stderr: str
    returncode: int
    duration_seconds: float
    top_level_tools: tuple[str, ...]
    nested_tools: tuple[str, ...]
    exec_inputs: tuple[str, ...]
    usages: tuple[dict[str, int], ...]


@dataclass(frozen=True)
class Case:
    name: str
    category: str
    prompt: str
    setup: Callable[[Path], None]
    grade: Callable[[Path, Observation], dict[str, bool]]


def write(path: Path, content: str, *, executable: bool = False) -> None:
    path.write_text(content, encoding="utf-8")
    if executable:
        path.chmod(0o755)


def setup_empty(_: Path) -> None:
    return


def setup_read_compute(root: Path) -> None:
    write(root / "numbers.txt", "17\n23\n41\n59\n")
    write(root / "label.txt", "quartz-orbit\n")


def grade_read_compute(_: Path, observation: Observation) -> dict[str, bool]:
    digest = hashlib.sha256(b"quartz-orbit\n").hexdigest()
    return {
        "completed": observation.returncode == 0,
        "used_exec_command": "exec_command" in observation.nested_tools,
        "reported_sum": "140" in observation.stdout,
        "reported_digest": digest in observation.stdout.lower(),
    }


def setup_patch_test(root: Path) -> None:
    write(
        root / "calculator.py",
        """def weighted_total(values):
    # The final value must be weighted by its one-based position too.
    return sum(index * value for index, value in enumerate(values))
""",
    )
    write(
        root / "test_calculator.py",
        """import unittest

from calculator import weighted_total


class WeightedTotalTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(weighted_total([]), 0)

    def test_positions_are_one_based(self):
        self.assertEqual(weighted_total([3, 5, 7]), 34)


if __name__ == "__main__":
    unittest.main()
""",
    )


def grade_patch_test(root: Path, observation: Observation) -> dict[str, bool]:
    tests = subprocess.run(
        ["python3", "-m", "unittest", "-q"],
        cwd=root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return {
        "completed": observation.returncode == 0,
        "used_apply_patch": "apply_patch" in observation.nested_tools,
        "used_exec_command": "exec_command" in observation.nested_tools,
        "tests_pass": tests.returncode == 0,
        "kept_test": (root / "test_calculator.py").exists(),
    }


def setup_interactive(root: Path) -> None:
    write(
        root / "challenge.py",
        """#!/usr/bin/env python3
import hashlib
import sys

if not sys.stdin.isatty():
    print("TTY_REQUIRED", flush=True)
    raise SystemExit(2)
print("TOKEN?", flush=True)
answer = input().strip()
print("RESULT=" + hashlib.sha256(answer.encode()).hexdigest()[:16], flush=True)
""",
        executable=True,
    )


def grade_interactive(_: Path, observation: Observation) -> dict[str, bool]:
    expected = hashlib.sha256(b"cobalt-otter").hexdigest()[:16]
    return {
        "completed": observation.returncode == 0,
        "used_exec_command": "exec_command" in observation.nested_tools,
        "used_write_stdin": "write_stdin" in observation.nested_tools,
        "reported_result": expected in observation.stdout.lower(),
        "did_not_report_tty_failure": "TTY_REQUIRED" not in observation.stdout,
    }


def png_chunk(kind: bytes, payload: bytes) -> bytes:
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload))


def setup_image(root: Path) -> None:
    width, height = 96, 64
    rows = []
    for y in range(height):
        row = bytearray([0])
        for x in range(width):
            if 40 <= x < 56 and 24 <= y < 40:
                color = (0, 0, 0)
            elif x < width // 2 and y < height // 2:
                color = (230, 30, 30)
            elif y < height // 2:
                color = (25, 180, 60)
            elif x < width // 2:
                color = (35, 80, 220)
            else:
                color = (245, 210, 25)
            row.extend(color)
        rows.append(bytes(row))
    payload = b"".join(rows)
    png = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + png_chunk(b"IDAT", zlib.compress(payload, 9))
        + png_chunk(b"IEND", b"")
    )
    (root / "quadrants.png").write_bytes(png)


def grade_image(_: Path, observation: Observation) -> dict[str, bool]:
    answer = observation.stdout.lower()
    return {
        "completed": observation.returncode == 0,
        "used_view_image": "view_image" in observation.nested_tools,
        "identified_upper_right": "green" in answer,
        "identified_center": "black" in answer,
    }


def setup_plan(root: Path) -> None:
    write(root / "alpha.txt", "alpha\n")
    write(root / "beta.txt", "stressed\n")
    write(root / "gamma.txt", "8 13 21\n")


def grade_plan(root: Path, observation: Observation) -> dict[str, bool]:
    return {
        "completed": observation.returncode == 0,
        "used_update_plan": "update_plan" in observation.nested_tools,
        "alpha_updated": (root / "alpha.txt").read_text() == "ALPHA\n",
        "beta_updated": (root / "beta.txt").read_text() == "desserts\n",
        "gamma_updated": (root / "gamma.txt").read_text() == "42\n",
    }


def grade_papercut(root: Path, observation: Observation) -> dict[str, bool]:
    path = root / "PAPERCUTS.md"
    content = path.read_text(encoding="utf-8") if path.exists() else ""
    expected = "Repeated setup checks hide the required compiler version; document it beside bootstrap."
    return {
        "completed": observation.returncode == 0,
        "used_log_papercut": observation.nested_tools.count("log_papercut") == 1,
        "created_expected_file": path.exists(),
        "recorded_exact_message_once": content.count(expected) == 1,
    }


def markdown_link_to(domain: str, text: str) -> bool:
    return re.search(rf"\[[^\]]+\]\(https://[^)\s]*{re.escape(domain)}[^)]*\)", text) is not None


def grade_web_primary(_: Path, observation: Observation) -> dict[str, bool]:
    answer = observation.stdout.lower()
    return {
        "completed": observation.returncode == 0,
        "used_web": "web__run" in observation.nested_tools,
        "cited_official_source": markdown_link_to("developers.openai.com", observation.stdout),
        "answered_topic": "concise" in answer or "lean" in answer or "description" in answer,
        "hid_internal_refs": re.search(r"turn\d+(?:search|view|fetch)\d+", answer) is None,
    }


def setup_local_policy(root: Path) -> None:
    write(root / "policy.txt", "release-codeword=maple-731\n")


def grade_no_web(_: Path, observation: Observation) -> dict[str, bool]:
    return {
        "completed": observation.returncode == 0,
        "did_not_use_web": "web__run" not in observation.nested_tools,
        "used_local_command": "exec_command" in observation.nested_tools,
        "reported_codeword": "maple-731" in observation.stdout.lower(),
    }


def grade_web_freshness(_: Path, observation: Observation) -> dict[str, bool]:
    answer = observation.stdout.lower()
    return {
        "completed": observation.returncode == 0,
        "used_web": "web__run" in observation.nested_tools,
        "cited_rust_source": markdown_link_to("rust-lang.org", observation.stdout),
        "reported_version_like_value": re.search(r"\b1\.\d{2,3}(?:\.\d+)?\b", answer) is not None,
        "hid_internal_refs": re.search(r"turn\d+(?:search|view|fetch)\d+", answer) is None,
    }


def setup_parallel(root: Path) -> None:
    write(root / "left.sh", "#!/bin/sh\nsleep 1\nprintf 'LEFT-17\\n'\n", executable=True)
    write(root / "right.sh", "#!/bin/sh\nsleep 1\nprintf 'RIGHT-29\\n'\n", executable=True)


def grade_parallel(_: Path, observation: Observation) -> dict[str, bool]:
    javascript = "\n".join(observation.exec_inputs)
    return {
        "completed": observation.returncode == 0,
        "used_two_commands": observation.nested_tools.count("exec_command") >= 2,
        "used_promise_all": "Promise.all" in javascript,
        "reported_left": "LEFT-17" in observation.stdout,
        "reported_right": "RIGHT-29" in observation.stdout,
    }


def setup_wait(root: Path) -> None:
    write(root / "slow.sh", "#!/bin/sh\nsleep 2\nprintf 'WAIT-READY-83\\n'\n", executable=True)


def grade_wait(_: Path, observation: Observation) -> dict[str, bool]:
    javascript = "\n".join(observation.exec_inputs)
    return {
        "completed": observation.returncode == 0,
        "used_exec_command": "exec_command" in observation.nested_tools,
        "used_exec_pragma": "@exec" in javascript and "yield_time_ms" in javascript,
        "used_wait": "wait" in observation.top_level_tools,
        "reported_result": "WAIT-READY-83" in observation.stdout,
    }


def grade_state_helpers(_: Path, observation: Observation) -> dict[str, bool]:
    javascript = "\n".join(observation.exec_inputs)
    return {
        "completed": observation.returncode == 0,
        "used_two_exec_cells": observation.top_level_tools.count("exec") >= 2,
        "used_store": re.search(r"\bstore\s*\(", javascript) is not None,
        "used_load": re.search(r"\bload\s*\(", javascript) is not None,
        "used_notify": re.search(r"\bnotify\s*\(", javascript) is not None,
        "reported_value": "STATE-619" in observation.stdout,
    }


CASES = (
    Case(
        "read_compute",
        "local_read",
        "Inspect numbers.txt and label.txt. Report the sum of every number and the full SHA-256 of label.txt. Do not modify files.",
        setup_read_compute,
        grade_read_compute,
    ),
    Case(
        "patch_and_test",
        "mutation",
        "Fix the defect in calculator.py so the existing tests pass. Use apply_patch for the edit and run the tests. Do not edit or replace the test file.",
        setup_patch_test,
        grade_patch_test,
    ),
    Case(
        "interactive_session",
        "session",
        "Run ./challenge.py in a persistent interactive TTY, answer its prompt with cobalt-otter using the existing session, and report only the RESULT value.",
        setup_interactive,
        grade_interactive,
    ),
    Case(
        "local_image",
        "image",
        "Inspect quadrants.png with the local image tool. State the upper-right quadrant color and the center square color. Do not infer them from file bytes.",
        setup_image,
        grade_image,
    ),
    Case(
        "planned_mutation",
        "planning",
        "Use update_plan to track three named phases while doing this work: uppercase alpha.txt, reverse beta.txt, and replace gamma.txt with the sum of its numbers. Validate all three results.",
        setup_plan,
        grade_plan,
    ),
    Case(
        "papercut_logging",
        "papercut",
        "Use log_papercut exactly once to record this message, without editing PAPERCUTS.md directly: Repeated setup checks hide the required compiler version; document it beside bootstrap. Then report the returned path.",
        setup_empty,
        grade_papercut,
    ),
    Case(
        "web_primary_citation",
        "web",
        "Research the current official OpenAI GPT-5.6 model guidance about leaner prompts and tool descriptions. Give its recommendation in one sentence with a direct Markdown citation. Use primary technical sources only.",
        setup_empty,
        grade_web_primary,
    ),
    Case(
        "local_no_web",
        "routing",
        "Do not browse or call any web operation. Read policy.txt locally and return only its release codeword.",
        setup_local_policy,
        grade_no_web,
    ),
    Case(
        "implicit_freshness_web",
        "routing",
        "What is the current stable Rust release? Verify the current value and answer with a direct Markdown citation to an official Rust source.",
        setup_empty,
        grade_web_freshness,
    ),
    Case(
        "parallel_commands",
        "orchestration",
        "Run left.sh and right.sh independently and concurrently in one exec JavaScript cell, then report both tokens. Use Promise.all rather than sequential calls.",
        setup_parallel,
        grade_parallel,
    ),
    Case(
        "yield_and_wait",
        "orchestration",
        "Run ./slow.sh in exec with a first-line pragma that yields within 250 ms. Then use the top-level wait tool until the yielded exec cell completes and report its token.",
        setup_wait,
        grade_wait,
    ),
    Case(
        "persistent_helpers",
        "runtime_helpers",
        "Use exactly two exec cells. In the first, store the string STATE-619 under key answer and call notify with saved. In the second, load answer and emit it with text. Return the loaded value.",
        setup_empty,
        grade_state_helpers,
    ),
)


def source_auth_path() -> Path:
    codex_home = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex"))
    path = codex_home / "auth.json"
    if not path.is_file():
        raise RuntimeError(f"Codex authentication is missing at {path}; run `codex login`")
    return path


def prepare_fixture(root: Path, case: Case) -> None:
    if root.exists():
        shutil.rmtree(root)
    root.mkdir(parents=True)
    write(root / "AGENTS.md", AGENTS)
    case.setup(root)
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)
    subprocess.run(["git", "add", "-A"], cwd=root, check=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=Tool Catalogue Eval",
            "-c",
            "user.email=eval@invalid",
            "commit",
            "-qm",
            "fixture",
        ],
        cwd=root,
        check=True,
    )


def extract_observation(
    session: Path,
    *,
    stdout: str,
    stderr: str,
    returncode: int,
    duration_seconds: float,
) -> Observation:
    items: list[dict[str, object]] = []
    usages: list[dict[str, int]] = []
    for line in session.read_text(encoding="utf-8").splitlines():
        record = json.loads(line)
        if record.get("type") in ("history_append", "history_replace"):
            items.extend(record.get("items", []))
        elif record.get("type") == "usage":
            usages.append(record["usage"])

    top_level_tools: list[str] = []
    exec_inputs: list[str] = []
    for item in items:
        kind = item.get("type")
        if kind in ("custom_tool_call", "function_call"):
            name = item.get("name")
            if isinstance(name, str):
                top_level_tools.append(name)
            if name == "exec" and isinstance(item.get("input"), str):
                exec_inputs.append(item["input"])
            elif name == "exec" and isinstance(item.get("arguments"), str):
                exec_inputs.append(item["arguments"])
    nested_tools = [
        match.group(1)
        for source in exec_inputs
        for match in re.finditer(r"\btools\.([A-Za-z_$][A-Za-z0-9_$]*)", source)
    ]
    return Observation(
        stdout=stdout,
        stderr=stderr,
        returncode=returncode,
        duration_seconds=duration_seconds,
        top_level_tools=tuple(top_level_tools),
        nested_tools=tuple(nested_tools),
        exec_inputs=tuple(exec_inputs),
        usages=tuple(usages),
    )


def run_case(
    binary: Path,
    case: Case,
    fixture: Path,
    state: Path,
) -> Observation:
    prepare_fixture(fixture, case)
    sessions = state / "bettercodex" / "sessions"
    before = set(sessions.glob("*.jsonl")) if sessions.exists() else set()
    environment = os.environ.copy()
    environment["CODEX_HOME"] = os.fspath(state)
    environment["BCODEX_HOME"] = os.fspath(state / "bcodex")
    started = time.monotonic()
    result = subprocess.run(
        [os.fspath(binary), case.prompt],
        cwd=fixture,
        env=environment,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=RUN_TIMEOUT_SECONDS,
    )
    duration = time.monotonic() - started
    created = set(sessions.glob("*.jsonl")) - before
    if len(created) != 1:
        raise RuntimeError(
            f"{case.name}: expected one new session journal, found {len(created)} in {sessions}"
        )
    return extract_observation(
        created.pop(),
        stdout=result.stdout,
        stderr=result.stderr,
        returncode=result.returncode,
        duration_seconds=duration,
    )


def parse_arm(value: str) -> tuple[str, Path]:
    name, separator, raw_path = value.partition("=")
    if not separator or not name or not raw_path:
        raise argparse.ArgumentTypeError("arms use NAME=/path/to/bcodex")
    path = Path(raw_path).expanduser().resolve()
    if not path.is_file() or not os.access(path, os.X_OK):
        raise argparse.ArgumentTypeError(f"arm binary is not executable: {path}")
    return name, path


def bounded(value: str) -> str:
    if len(value) <= MAX_RECORDED_OUTPUT_CHARS:
        return value
    half = MAX_RECORDED_OUTPUT_CHARS // 2
    return value[:half] + "\n...[evaluation output truncated]...\n" + value[-half:]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arm", action="append", required=True, type=parse_arm)
    parser.add_argument("--repetitions", type=int, default=DEFAULT_REPETITIONS)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--case", action="append", dest="selected_cases")
    arguments = parser.parse_args()
    if arguments.repetitions < 1:
        parser.error("--repetitions must be positive")
    arms = dict(arguments.arm)
    if len(arms) != len(arguments.arm):
        parser.error("arm names must be unique")
    known_cases = {case.name: case for case in CASES}
    selected = arguments.selected_cases or list(known_cases)
    unknown = sorted(set(selected) - set(known_cases))
    if unknown:
        parser.error(f"unknown cases: {', '.join(unknown)}")

    script_hash = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    schedule = [
        (known_cases[name], repetition)
        for repetition in range(1, arguments.repetitions + 1)
        for name in selected
    ]
    random.Random(SEED).shuffle(schedule)
    auth = source_auth_path()
    results: list[dict[str, object]] = []
    with tempfile.TemporaryDirectory(prefix="bettercodex-tool-eval-") as temporary:
        temporary_root = Path(temporary)
        fixture = temporary_root / "fixture"
        states: dict[str, Path] = {}
        for arm in arms:
            state = temporary_root / f"state-{arm}"
            state.mkdir(mode=0o700)
            shutil.copy2(auth, state / "auth.json")
            (state / "auth.json").chmod(0o600)
            states[arm] = state

        for case, repetition in schedule:
            arm_order = list(arms)
            random.Random(f"{SEED}:{case.name}:{repetition}").shuffle(arm_order)
            for arm in arm_order:
                print(f"[{len(results) + 1}/{len(schedule) * len(arms)}] {case.name} r{repetition} {arm}", flush=True)
                observation = run_case(arms[arm], case, fixture, states[arm])
                checks = case.grade(fixture, observation)
                usage_totals = {
                    key: sum(usage.get(key, 0) for usage in observation.usages)
                    for key in (
                        "input_tokens",
                        "cached_input_tokens",
                        "cache_write_input_tokens",
                        "output_tokens",
                        "reasoning_output_tokens",
                        "total_tokens",
                    )
                }
                results.append(
                    {
                        "arm": arm,
                        "binary_sha256": hashlib.sha256(arms[arm].read_bytes()).hexdigest(),
                        "case": case.name,
                        "category": case.category,
                        "repetition": repetition,
                        "passed": all(checks.values()),
                        "checks": checks,
                        "duration_seconds": round(observation.duration_seconds, 3),
                        "top_level_tools": observation.top_level_tools,
                        "nested_tools": observation.nested_tools,
                        "exec_inputs": observation.exec_inputs,
                        "usage_totals": usage_totals,
                        "request_input_tokens": [
                            usage.get("input_tokens", 0) for usage in observation.usages
                        ],
                        "stdout": bounded(observation.stdout),
                        "stderr": bounded(observation.stderr),
                    }
                )

    summary = {}
    for arm in arms:
        arm_results = [result for result in results if result["arm"] == arm]
        summary[arm] = {
            "passed": sum(bool(result["passed"]) for result in arm_results),
            "total": len(arm_results),
            "input_tokens": sum(result["usage_totals"]["input_tokens"] for result in arm_results),
            "output_tokens": sum(result["usage_totals"]["output_tokens"] for result in arm_results),
            "reasoning_output_tokens": sum(
                result["usage_totals"]["reasoning_output_tokens"] for result in arm_results
            ),
            "duration_seconds": round(sum(result["duration_seconds"] for result in arm_results), 3),
        }
    document = {
        "protocol": {
            "script_sha256": script_hash,
            "seed": SEED,
            "repetitions": arguments.repetitions,
            "cases": selected,
            "acceptance": {
                "minimum_catalogue_token_reduction_percent": 35,
                "candidate_total_passes_must_not_be_lower": True,
                "candidate_case_passes_must_not_be_lower": True,
                "tie_break": "If either arm differs by one pass in a case, run one additional matched repetition for that case and retain every result.",
            },
        },
        "summary": summary,
        "results": results,
    }
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))
    print(f"Results: {arguments.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
