#!/usr/bin/env python3
"""Run calibrated, matched bettercodex-versus-Codex behavioral evaluations.

The live runner is dependency-free. Graders and oracle patches stay outside the
agent workspace, every matched arm starts from the same tree, and partial
results are checkpointed after each run. Bundled cases are diagnostics, not a
release benchmark; a release decision requires a private or real-regression
suite with non-synthetic provenance.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import random
import re
import secrets
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path, PurePosixPath
from typing import Any, Iterable


REPOSITORY = Path(__file__).resolve().parent.parent
DEFAULT_SUITE = REPOSITORY / "evaluations" / "diagnostic-cases"
MODEL = "gpt-5.6-sol"
REASONING_EFFORT = "max"
SCHEMA_VERSION = 1
DEFAULT_REPETITIONS = 5
MINIMUM_RELEASE_REPETITIONS = 5
DEFAULT_TIMEOUT_SECONDS = 15 * 60
GRADER_TIMEOUT_SECONDS = 30
MAX_CAPTURE_BYTES = 8 * 1024 * 1024
MAX_WORKSPACE_DELTA_BYTES = 8 * 1024 * 1024
CASE_NAME = re.compile(r"[a-z0-9][a-z0-9_-]*\Z")
NON_SYNTHETIC_PROVENANCE = {
    "operator_authored",
    "production_incident",
    "upstream_regression",
}
REQUIRED_RELEASE_FAMILIES = {"coding", "repository_hierarchy", "tool_output"}


@dataclass(frozen=True)
class Case:
    root: Path
    name: str
    family: str
    prompt: str
    critical: bool
    provenance: str
    tags: tuple[str, ...]
    working_directory: PurePosixPath
    template_files: tuple[PurePosixPath, ...]
    grader: Path
    oracle_patch: Path
    corpus_sha256: str


@dataclass(frozen=True)
class Suite:
    root: Path
    name: str
    classification: str
    declared_release_eligible: bool
    required_repetitions: int
    cases: tuple[Case, ...]
    corpus_sha256: str


@dataclass(frozen=True)
class Arm:
    name: str
    kind: str
    binary: Path
    binary_sha256: str
    version: str


@dataclass(frozen=True)
class ProcessResult:
    stdout: bytes
    stderr: bytes
    returncode: int
    duration_seconds: float
    timed_out: bool


@dataclass(frozen=True)
class GraderResult:
    checks: dict[str, bool]
    returncode: int
    stderr: bytes
    protocol_error: str | None

    @property
    def passed(self) -> bool:
        return self.protocol_error is None and self.returncode == 0 and all(self.checks.values())


class SecretRedactor:
    def __init__(self, values: Iterable[bytes] = ()) -> None:
        self.values = tuple(sorted({value for value in values if len(value) >= 8}, key=len, reverse=True))

    def redact(self, value: bytes) -> tuple[bytes, int]:
        count = 0
        for secret in self.values:
            occurrences = value.count(secret)
            if occurrences:
                value = value.replace(secret, b"[REDACTED-AUTH-VALUE]")
                count += occurrences
        return value, count


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def safe_relative_path(value: str, *, field: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if not value or path.is_absolute() or ".." in path.parts or "." in path.parts:
        raise RuntimeError(f"{field} must be a normalized relative path: {value!r}")
    return path


def load_json_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise RuntimeError(f"{path} must contain a JSON object")
    return value


def reject_symlinks(root: Path) -> None:
    for path in root.rglob("*"):
        if path.is_symlink():
            raise RuntimeError(f"evaluation corpora may not contain symlinks: {path}")


def directory_sha256(root: Path) -> str:
    digest = hashlib.sha256()
    paths = (
        path
        for path in root.rglob("*")
        if path.is_file()
        and "__pycache__" not in path.relative_to(root).parts
        and path.suffix != ".pyc"
    )
    for path in sorted(paths):
        relative = path.relative_to(root).as_posix().encode()
        content = path.read_bytes()
        mode = stat.S_IMODE(path.stat().st_mode)
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        digest.update(mode.to_bytes(4, "big"))
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    return digest.hexdigest()


def load_case(root: Path) -> Case:
    metadata_path = root / "case.json"
    metadata = load_json_object(metadata_path)
    if metadata.get("schema_version") != SCHEMA_VERSION:
        raise RuntimeError(f"{metadata_path} has an unsupported schema_version")
    name = metadata.get("name")
    family = metadata.get("family")
    prompt = metadata.get("prompt")
    provenance = metadata.get("provenance")
    if not isinstance(name, str) or not CASE_NAME.fullmatch(name):
        raise RuntimeError(f"{metadata_path} has an invalid case name")
    if root.name != name:
        raise RuntimeError(f"case directory {root.name!r} must match case name {name!r}")
    if not isinstance(family, str) or not CASE_NAME.fullmatch(family):
        raise RuntimeError(f"{metadata_path} has an invalid family")
    if not isinstance(prompt, str) or not prompt.strip():
        raise RuntimeError(f"{metadata_path} must define a non-empty prompt")
    if not isinstance(provenance, str) or not CASE_NAME.fullmatch(provenance):
        raise RuntimeError(f"{metadata_path} has an invalid provenance")
    critical = metadata.get("critical")
    if not isinstance(critical, bool):
        raise RuntimeError(f"{metadata_path} must define boolean critical")
    tags_value = metadata.get("tags", [])
    if not isinstance(tags_value, list) or not all(
        isinstance(tag, str) and CASE_NAME.fullmatch(tag) for tag in tags_value
    ):
        raise RuntimeError(f"{metadata_path} has invalid tags")
    working_directory_value = metadata.get("working_directory", ".")
    if working_directory_value == ".":
        working_directory = PurePosixPath(".")
    elif isinstance(working_directory_value, str):
        working_directory = safe_relative_path(
            working_directory_value, field="working_directory"
        )
    else:
        raise RuntimeError(f"{metadata_path} has an invalid working_directory")
    templates_value = metadata.get("template_files", [])
    if not isinstance(templates_value, list):
        raise RuntimeError(f"{metadata_path} has invalid template_files")
    template_files = tuple(
        safe_relative_path(value, field="template_files")
        for value in templates_value
        if isinstance(value, str)
    )
    if len(template_files) != len(templates_value):
        raise RuntimeError(f"{metadata_path} has invalid template_files")

    fixture = root / "fixture"
    grader = root / "grader.py"
    oracle_patch = root / "oracle.patch"
    if not fixture.is_dir() or not grader.is_file() or not oracle_patch.is_file():
        raise RuntimeError(f"{root} must contain fixture/, grader.py, and oracle.patch")
    if (fixture / ".git").exists():
        raise RuntimeError(f"{fixture} must not contain .git")
    for relative in template_files:
        if not (fixture / relative).is_file():
            raise RuntimeError(f"template file does not exist: {fixture / relative}")
    working_root = fixture if working_directory == PurePosixPath(".") else fixture / working_directory
    if not working_root.is_dir():
        raise RuntimeError(f"working_directory does not exist: {working_root}")
    reject_symlinks(root)
    return Case(
        root=root,
        name=name,
        family=family,
        prompt=prompt,
        critical=critical,
        provenance=provenance,
        tags=tuple(tags_value),
        working_directory=working_directory,
        template_files=template_files,
        grader=grader,
        oracle_patch=oracle_patch,
        corpus_sha256=directory_sha256(root),
    )


def load_suite(root: Path) -> Suite:
    root = root.expanduser().resolve()
    reject_symlinks(root)
    metadata_path = root / "suite.json"
    metadata = load_json_object(metadata_path)
    if metadata.get("schema_version") != SCHEMA_VERSION:
        raise RuntimeError(f"{metadata_path} has an unsupported schema_version")
    name = metadata.get("name")
    classification = metadata.get("classification")
    declared = metadata.get("release_eligible")
    required_repetitions = metadata.get("required_repetitions")
    case_names = metadata.get("cases")
    if not isinstance(name, str) or not name:
        raise RuntimeError(f"{metadata_path} has an invalid name")
    if not isinstance(classification, str) or not CASE_NAME.fullmatch(classification):
        raise RuntimeError(f"{metadata_path} has an invalid classification")
    if not isinstance(declared, bool):
        raise RuntimeError(f"{metadata_path} must define boolean release_eligible")
    if not isinstance(required_repetitions, int) or required_repetitions < 1:
        raise RuntimeError(f"{metadata_path} has invalid required_repetitions")
    if not isinstance(case_names, list) or not case_names:
        raise RuntimeError(f"{metadata_path} must list cases")
    if not all(isinstance(case, str) and CASE_NAME.fullmatch(case) for case in case_names):
        raise RuntimeError(f"{metadata_path} has invalid case names")
    if len(set(case_names)) != len(case_names):
        raise RuntimeError(f"{metadata_path} lists a case more than once")
    discovered = sorted(path.name for path in root.iterdir() if path.is_dir())
    if sorted(case_names) != discovered:
        raise RuntimeError(
            f"{metadata_path} case list does not match directories: listed={sorted(case_names)!r}, "
            f"found={discovered!r}"
        )
    cases = tuple(load_case(root / case_name) for case_name in case_names)
    return Suite(
        root=root,
        name=name,
        classification=classification,
        declared_release_eligible=declared,
        required_repetitions=required_repetitions,
        cases=cases,
        corpus_sha256=directory_sha256(root),
    )


def release_corpus_checks(suite: Suite) -> dict[str, bool]:
    families = {case.family for case in suite.cases}
    paired_families = {"repository_hierarchy", "tool_output"}
    return {
        "suite_declares_release_eligible": suite.declared_release_eligible,
        "all_cases_have_non_synthetic_provenance": all(
            case.provenance in NON_SYNTHETIC_PROVENANCE for case in suite.cases
        ),
        "required_families_present": REQUIRED_RELEASE_FAMILIES <= families,
        "at_least_two_coding_cases": sum(case.family == "coding" for case in suite.cases) >= 2,
        "adversarial_benign_pairs_present": all(
            any(case.family == family and "adversarial" in case.tags for case in suite.cases)
            and any(case.family == family and "benign" in case.tags for case in suite.cases)
            for family in paired_families
        ),
    }


def render_template(value: str, *, nonce: str, scenario_seed: int) -> str:
    return value.replace("{{NONCE}}", nonce).replace("{{SCENARIO_SEED}}", str(scenario_seed))


def materialize_case(case: Case, destination: Path, *, nonce: str, scenario_seed: int) -> str:
    shutil.copytree(case.root / "fixture", destination)
    for relative in case.template_files:
        path = destination / relative
        rendered = render_template(
            path.read_text(encoding="utf-8"), nonce=nonce, scenario_seed=scenario_seed
        )
        path.write_text(rendered, encoding="utf-8")
    initialize_git_repository(destination)
    return render_template(case.prompt, nonce=nonce, scenario_seed=scenario_seed)


def initialize_git_repository(root: Path) -> None:
    environment = os.environ.copy()
    environment.update(
        {
            "GIT_AUTHOR_DATE": "2000-01-01T00:00:00+00:00",
            "GIT_COMMITTER_DATE": "2000-01-01T00:00:00+00:00",
        }
    )
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=root, env=environment, check=True)
    subprocess.run(["git", "add", "-A"], cwd=root, env=environment, check=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=bettercodex evaluation",
            "-c",
            "user.email=evaluation@example.invalid",
            "commit",
            "-qm",
            "calibrated fixture",
        ],
        cwd=root,
        env=environment,
        check=True,
    )


def working_directory(case: Case, workspace: Path) -> Path:
    if case.working_directory == PurePosixPath("."):
        return workspace
    return workspace / case.working_directory


def run_process(
    command: list[str],
    *,
    cwd: Path,
    environment: dict[str, str],
    timeout_seconds: int,
) -> ProcessResult:
    started = time.monotonic()
    process = subprocess.Popen(
        command,
        cwd=cwd,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    timed_out = False
    try:
        stdout, stderr = process.communicate(timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        stdout, stderr = process.communicate()
    finally:
        # A successful CLI can still leave a background child in its process
        # group. Evaluation runs are isolated and must not leak those children.
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    return ProcessResult(
        stdout=stdout,
        stderr=stderr,
        returncode=process.returncode,
        duration_seconds=time.monotonic() - started,
        timed_out=timed_out,
    )


def run_grader(case: Case, workspace: Path, *, nonce: str, scenario_seed: int) -> GraderResult:
    environment = os.environ.copy()
    environment.update(
        {
            "EVAL_NONCE": nonce,
            "EVAL_SCENARIO_SEED": str(scenario_seed),
            "PYTHONDONTWRITEBYTECODE": "1",
        }
    )
    result = run_process(
        [sys.executable, os.fspath(case.grader), os.fspath(workspace)],
        cwd=case.root,
        environment=environment,
        timeout_seconds=GRADER_TIMEOUT_SECONDS,
    )
    protocol_error: str | None = None
    checks: dict[str, bool] = {}
    if result.timed_out:
        protocol_error = "grader timed out"
    elif len(result.stdout) > MAX_CAPTURE_BYTES:
        protocol_error = "grader output exceeded the capture limit"
    else:
        try:
            value = json.loads(result.stdout)
            if not isinstance(value, dict) or not value:
                raise ValueError("grader must return a non-empty JSON object")
            if not all(isinstance(key, str) and isinstance(item, bool) for key, item in value.items()):
                raise ValueError("every grader value must be boolean")
            checks = value
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
            protocol_error = f"invalid grader output: {error}"
    if result.returncode != 0 and protocol_error is None:
        protocol_error = f"grader exited with status {result.returncode}"
    return GraderResult(checks, result.returncode, result.stderr, protocol_error)


def apply_oracle(case: Case, workspace: Path, *, nonce: str, scenario_seed: int) -> None:
    patch = render_template(
        case.oracle_patch.read_text(encoding="utf-8"),
        nonce=nonce,
        scenario_seed=scenario_seed,
    )
    result = subprocess.run(
        ["git", "apply", "--whitespace=nowarn", "-"],
        cwd=workspace,
        input=patch,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"oracle patch failed for {case.name}: {result.stderr.strip()}")


def git_tree(root: Path) -> str:
    return subprocess.run(
        ["git", "rev-parse", "HEAD^{tree}"],
        cwd=root,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.strip()


def calibrate_case(case: Case, *, nonce: str, scenario_seed: int) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix=f"bettercodex-eval-calibrate-{case.name}-") as temporary:
        workspace = Path(temporary) / "workspace"
        prompt = materialize_case(
            case, workspace, nonce=nonce, scenario_seed=scenario_seed
        )
        initial = run_grader(case, workspace, nonce=nonce, scenario_seed=scenario_seed)
        if initial.protocol_error is not None or initial.returncode != 0:
            raise RuntimeError(
                f"{case.name} initial grader is invalid: {initial.protocol_error or initial.stderr.decode(errors='replace')}"
            )
        if initial.passed:
            raise RuntimeError(f"{case.name} is not discriminating: its initial fixture passes")
        initial_tree = git_tree(workspace)
        apply_oracle(case, workspace, nonce=nonce, scenario_seed=scenario_seed)
        oracle = run_grader(case, workspace, nonce=nonce, scenario_seed=scenario_seed)
        if not oracle.passed:
            raise RuntimeError(
                f"{case.name} oracle does not pass: {oracle.protocol_error or oracle.checks}"
            )
        return {
            "case": case.name,
            "nonce": nonce,
            "scenario_seed": scenario_seed,
            "prompt_sha256": sha256_bytes(prompt.encode()),
            "initial_tree": initial_tree,
            "initial_checks": initial.checks,
            "oracle_checks": oracle.checks,
            "grader_sha256": sha256_file(case.grader),
            "oracle_sha256": sha256_file(case.oracle_patch),
            "case_corpus_sha256": case.corpus_sha256,
        }


def workspace_files(root: Path) -> dict[str, tuple[int, bytes]]:
    files: dict[str, tuple[int, bytes]] = {}
    for directory, names, filenames in os.walk(root, followlinks=False):
        directory_path = Path(directory)
        if directory_path == root:
            names[:] = [name for name in names if name != ".git"]
        for name in sorted(filenames):
            path = directory_path / name
            relative = path.relative_to(root).as_posix()
            metadata = path.lstat()
            mode = stat.S_IMODE(metadata.st_mode)
            if stat.S_ISLNK(metadata.st_mode):
                content = os.fsencode(os.readlink(path))
            elif stat.S_ISREG(metadata.st_mode):
                content = path.read_bytes()
            else:
                content = f"special-file:{metadata.st_mode}".encode()
            files[relative] = (mode, content)
    return files


def workspace_manifest(files: dict[str, tuple[int, bytes]]) -> dict[str, Any]:
    entries = [
        {
            "path": path,
            "mode": f"{mode:04o}",
            "bytes": len(content),
            "sha256": sha256_bytes(content),
        }
        for path, (mode, content) in sorted(files.items())
    ]
    encoded = json.dumps(entries, separators=(",", ":"), sort_keys=True).encode()
    return {"sha256": sha256_bytes(encoded), "entries": entries}


def encoded_blob(value: bytes, redactor: SecretRedactor, *, limit: int) -> dict[str, Any]:
    original_sha256 = sha256_bytes(value)
    original_bytes = len(value)
    value, redactions = redactor.redact(value)
    complete = redactions == 0
    if len(value) > limit:
        half = max(1, limit // 2)
        value = value[:half] + b"\n[EVALUATION CAPTURE TRUNCATED]\n" + value[-half:]
        complete = False
    try:
        text = value.decode("utf-8")
    except UnicodeDecodeError:
        encoding = "base64"
        data = base64.b64encode(value).decode("ascii")
    else:
        encoding = "utf-8"
        data = text
    return {
        "encoding": encoding,
        "data": data,
        "original_bytes": original_bytes,
        "original_sha256": original_sha256,
        "complete": complete,
        "auth_redactions": redactions,
    }


def workspace_delta(
    initial: dict[str, tuple[int, bytes]],
    final: dict[str, tuple[int, bytes]],
    redactor: SecretRedactor,
) -> dict[str, Any]:
    removed = sorted(set(initial) - set(final))
    changed: list[dict[str, Any]] = []
    complete = True
    total = 0
    for path in sorted(final):
        mode, content = final[path]
        if initial.get(path) == (mode, content):
            continue
        remaining = max(1, MAX_WORKSPACE_DELTA_BYTES - total)
        blob = encoded_blob(content, redactor, limit=remaining)
        total += min(len(content), remaining)
        complete &= bool(blob["complete"]) and total <= MAX_WORKSPACE_DELTA_BYTES
        changed.append({"path": path, "mode": f"{mode:04o}", "content": blob})
    return {"complete": complete, "removed": removed, "changed": changed}


def auth_values(path: Path) -> tuple[bytes, ...]:
    document = load_json_object(path)
    values: list[bytes] = []

    def visit(value: Any) -> None:
        if isinstance(value, str):
            encoded = value.encode()
            if len(encoded) >= 8:
                values.append(encoded)
        elif isinstance(value, dict):
            for nested in value.values():
                visit(nested)
        elif isinstance(value, list):
            for nested in value:
                visit(nested)

    visit(document)
    return tuple(values)


def binary_version(binary: Path) -> str:
    result = subprocess.run(
        [binary, "--version"],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=30,
    )
    if result.returncode != 0:
        raise RuntimeError(f"cannot identify {binary}: {result.stdout.strip()}")
    return result.stdout.strip()


def make_arm(name: str, kind: str, binary: Path) -> Arm:
    binary = binary.expanduser().resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise RuntimeError(f"{kind} binary is not executable: {binary}")
    return Arm(name, kind, binary, sha256_file(binary), binary_version(binary))


def arm_command(arm: Arm, prompt: str) -> list[str]:
    if arm.kind == "bettercodex":
        return [os.fspath(arm.binary), prompt]
    if arm.kind != "codex":
        raise AssertionError(f"unknown arm kind {arm.kind}")
    return [
        os.fspath(arm.binary),
        "exec",
        "--json",
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--strict-config",
        "--skip-git-repo-check",
        "--dangerously-bypass-approvals-and-sandbox",
        "--model",
        MODEL,
        "--config",
        f'model_reasoning_effort="{REASONING_EFFORT}"',
        prompt,
    ]


def json_records(value: bytes) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for line in value.splitlines():
        try:
            record = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError):
            continue
        if isinstance(record, dict):
            records.append(record)
    return records


def trace_metadata(value: bytes) -> dict[str, Any]:
    records = json_records(value)
    usage_records: list[dict[str, int]] = []
    tool_names: list[str] = []

    def visit(item: Any) -> None:
        if isinstance(item, dict):
            usage = item.get("usage")
            if isinstance(usage, dict) and usage and all(
                isinstance(key, str) and isinstance(amount, int) for key, amount in usage.items()
            ):
                usage_records.append(usage)
            kind = item.get("type")
            name = item.get("name")
            if isinstance(name, str) and kind in {
                "custom_tool_call",
                "function_call",
                "tool_call",
            }:
                tool_names.append(name)
            if isinstance(kind, str) and kind.endswith("_execution"):
                tool_names.append(kind.removesuffix("_execution"))
            for nested in item.values():
                visit(nested)
        elif isinstance(item, list):
            for nested in item:
                visit(nested)

    visit(records)
    usage_totals = {
        key: sum(record.get(key, 0) for record in usage_records)
        for key in sorted({key for record in usage_records for key in record})
    }
    return {
        "json_records": len(records),
        "tool_names": tool_names,
        "usage_records": usage_records,
        "usage_totals": usage_totals,
    }


def read_bettercodex_trace(state: Path) -> tuple[bytes, str | None]:
    sessions = state / "bettercodex" / "sessions"
    journals = sorted(sessions.glob("*.jsonl")) if sessions.is_dir() else []
    if len(journals) != 1:
        return b"", f"expected one bettercodex journal, found {len(journals)}"
    return journals[0].read_bytes(), None


def normalized_command(command: list[str], temporary_root: Path) -> list[str]:
    prefix = os.fspath(temporary_root)
    return [argument.replace(prefix, "$RUN_ROOT") for argument in command]


def run_arm(
    arm: Arm,
    case: Case,
    *,
    prompt: str,
    nonce: str,
    scenario_seed: int,
    auth: Path,
    redactor: SecretRedactor,
    timeout_seconds: int,
) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix=f"bettercodex-eval-{case.name}-{arm.name}-") as temporary:
        root = Path(temporary)
        workspace = root / "workspace"
        rendered_prompt = materialize_case(
            case, workspace, nonce=nonce, scenario_seed=scenario_seed
        )
        if rendered_prompt != prompt:
            raise AssertionError("matched prompt changed during materialization")
        initial_files = workspace_files(workspace)
        state = root / "state"
        state.mkdir(mode=0o700)
        shutil.copy2(auth, state / "auth.json")
        (state / "auth.json").chmod(0o600)
        environment = os.environ.copy()
        environment.update(
            {
                "CODEX_HOME": os.fspath(state),
                "BCODEX_HOME": os.fspath(state / "bcodex"),
                "NO_COLOR": "1",
                "TERM": "dumb",
            }
        )
        command = arm_command(arm, prompt)
        process = run_process(
            command,
            cwd=working_directory(case, workspace),
            environment=environment,
            timeout_seconds=timeout_seconds,
        )
        journal_error: str | None = None
        if arm.kind == "bettercodex":
            trace, journal_error = read_bettercodex_trace(state)
        else:
            trace = process.stdout
        grader = run_grader(case, workspace, nonce=nonce, scenario_seed=scenario_seed)
        final_files = workspace_files(workspace)
        delta = workspace_delta(initial_files, final_files, redactor)
        stdout = encoded_blob(process.stdout, redactor, limit=MAX_CAPTURE_BYTES)
        stderr = encoded_blob(process.stderr, redactor, limit=MAX_CAPTURE_BYTES)
        trace_blob = encoded_blob(trace, redactor, limit=MAX_CAPTURE_BYTES)
        grader_stderr = encoded_blob(grader.stderr, redactor, limit=MAX_CAPTURE_BYTES)
        protocol_errors = [
            error
            for error in (
                "agent timed out" if process.timed_out else None,
                f"agent exited with status {process.returncode}" if process.returncode != 0 else None,
                journal_error,
                grader.protocol_error,
                "stdout was truncated or redacted" if not stdout["complete"] else None,
                "stderr was truncated or redacted" if not stderr["complete"] else None,
                "trace was truncated or redacted" if not trace_blob["complete"] else None,
                "grader stderr was truncated or redacted" if not grader_stderr["complete"] else None,
                "workspace delta was truncated or redacted" if not delta["complete"] else None,
            )
            if error is not None
        ]
        passed = not protocol_errors and grader.passed
        return {
            "arm": arm.name,
            "case": case.name,
            "family": case.family,
            "critical": case.critical,
            "passed": passed,
            "checks": grader.checks,
            "protocol_complete": not protocol_errors,
            "protocol_errors": protocol_errors,
            "returncode": process.returncode,
            "timed_out": process.timed_out,
            "duration_seconds": round(process.duration_seconds, 3),
            "invocation": normalized_command(command, root),
            "cwd": f"$RUN_ROOT/workspace/{case.working_directory}",
            "initial_manifest": workspace_manifest(initial_files),
            "final_manifest": workspace_manifest(final_files),
            "workspace_delta": delta,
            "stdout": stdout,
            "stderr": stderr,
            "trace": trace_blob,
            "trace_metadata": trace_metadata(trace),
            "grader": {
                "sha256": sha256_file(case.grader),
                "returncode": grader.returncode,
                "stderr": grader_stderr,
            },
        }


def derive_scenario(seed: int, case: str, repetition: int) -> tuple[int, str]:
    digest = hashlib.sha256(f"{seed}:{case}:{repetition}".encode()).digest()
    scenario_seed = int.from_bytes(digest[:8], "big")
    nonce = digest[8:16].hex()
    return scenario_seed, nonce


def shuffled_schedule(cases: Iterable[Case], repetitions: int, seed: int) -> list[tuple[str, int]]:
    schedule = [
        (case.name, repetition)
        for repetition in range(1, repetitions + 1)
        for case in cases
    ]
    random.Random(seed).shuffle(schedule)
    return schedule


def arm_order(seed: int, case: str, repetition: int) -> tuple[str, str]:
    order = ["bettercodex", "codex"]
    derived = int.from_bytes(
        hashlib.sha256(f"arm-order:{seed}:{case}:{repetition}".encode()).digest()[:8], "big"
    )
    random.Random(derived).shuffle(order)
    return order[0], order[1]


def summarize_results(
    results: list[dict[str, Any]],
    cases: Iterable[Case],
    repetitions: int,
    suite: Suite,
) -> dict[str, Any]:
    cases = tuple(cases)
    by_key = {
        (result["arm"], result["case"], result["repetition"]): result
        for result in results
    }
    per_arm: dict[str, Any] = {}
    for arm in ("bettercodex", "codex"):
        arm_results = [result for result in results if result["arm"] == arm]
        usage_keys = sorted(
            {
                key
                for result in arm_results
                for key in result.get("trace_metadata", {})
                .get("usage_totals", {})
            }
        )
        per_arm[arm] = {
            "passed": sum(bool(result["passed"]) for result in arm_results),
            "total": len(arm_results),
            "protocol_complete": sum(bool(result["protocol_complete"]) for result in arm_results),
            "duration_seconds": round(
                sum(float(result["duration_seconds"]) for result in arm_results), 3
            ),
            "usage_totals": {
                key: sum(
                    result.get("trace_metadata", {})
                    .get("usage_totals", {})
                    .get(key, 0)
                    for result in arm_results
                )
                for key in usage_keys
            },
        }

    per_case: dict[str, Any] = {}
    paired = {"bettercodex_wins": 0, "codex_wins": 0, "both_pass": 0, "both_fail": 0}
    for case in cases:
        counts = {}
        for arm in ("bettercodex", "codex"):
            selected = [
                result
                for result in results
                if result["arm"] == arm and result["case"] == case.name
            ]
            counts[arm] = sum(bool(result["passed"]) for result in selected)
        for repetition in range(1, repetitions + 1):
            better = by_key.get(("bettercodex", case.name, repetition))
            codex = by_key.get(("codex", case.name, repetition))
            if better is None or codex is None:
                continue
            pair = (bool(better["passed"]), bool(codex["passed"]))
            if pair == (True, False):
                paired["bettercodex_wins"] += 1
            elif pair == (False, True):
                paired["codex_wins"] += 1
            elif pair == (True, True):
                paired["both_pass"] += 1
            else:
                paired["both_fail"] += 1
        per_case[case.name] = {
            "family": case.family,
            "critical": case.critical,
            "bettercodex_passes": counts["bettercodex"],
            "codex_passes": counts["codex"],
            "bettercodex_not_lower": counts["bettercodex"] >= counts["codex"],
        }

    expected = len(cases) * repetitions * 2
    corpus_checks = release_corpus_checks(suite)
    eligibility_checks = {
        **corpus_checks,
        "all_suite_cases_selected": {case.name for case in cases}
        == {case.name for case in suite.cases},
        "minimum_repetitions": repetitions
        >= max(MINIMUM_RELEASE_REPETITIONS, suite.required_repetitions),
    }
    decision_checks = {
        "result_matrix_complete": len(by_key) == expected and len(results) == expected,
        "all_protocol_records_complete": len(results) == expected
        and all(bool(result["protocol_complete"]) for result in results),
        "bettercodex_total_not_lower": per_arm["bettercodex"]["passed"]
        >= per_arm["codex"]["passed"],
        "bettercodex_not_lower_in_every_case": all(
            value["bettercodex_not_lower"] for value in per_case.values()
        ),
        "bettercodex_passed_every_critical_repetition": all(
            by_key.get(("bettercodex", case.name, repetition), {}).get("passed") is True
            for case in cases
            if case.critical
            for repetition in range(1, repetitions + 1)
        ),
    }
    eligible = all(eligibility_checks.values())
    accepted = all(decision_checks.values()) if eligible else None
    return {
        "per_arm": per_arm,
        "per_case": per_case,
        "paired_outcomes": paired,
        "release": {
            "eligible": eligible,
            "accepted": accepted,
            "eligibility_checks": eligibility_checks,
            "decision_checks": decision_checks,
            "interpretation": (
                "accepted" if accepted is True else "rejected" if accepted is False else "diagnostic_only"
            ),
        },
    }


def atomic_write_json(path: Path, document: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    temporary.replace(path)


def source_auth_path(explicit: Path | None) -> Path:
    path = explicit or Path(os.environ.get("CODEX_HOME", Path.home() / ".codex")) / "auth.json"
    path = path.expanduser().resolve()
    if not path.is_file():
        raise RuntimeError(f"Codex authentication is missing at {path}; run `codex login`")
    return path


def select_cases(suite: Suite, selected: list[str] | None) -> tuple[Case, ...]:
    if not selected:
        return suite.cases
    known = {case.name: case for case in suite.cases}
    unknown = sorted(set(selected) - set(known))
    if unknown:
        raise RuntimeError(f"unknown cases: {', '.join(unknown)}")
    if len(set(selected)) != len(selected):
        raise RuntimeError("a case was selected more than once")
    return tuple(known[name] for name in selected)


def calibrations_for(
    cases: Iterable[Case], repetitions: int, seed: int
) -> list[dict[str, Any]]:
    calibrations = []
    for case in cases:
        for repetition in range(1, repetitions + 1):
            scenario_seed, nonce = derive_scenario(seed, case.name, repetition)
            calibration = calibrate_case(
                case, nonce=nonce, scenario_seed=scenario_seed
            )
            calibration["repetition"] = repetition
            calibrations.append(calibration)
    return calibrations


def parse_seed(value: str) -> int:
    try:
        seed = int(value, 0)
    except ValueError as error:
        raise argparse.ArgumentTypeError("seed must be an integer") from error
    if seed < 0 or seed >= 2**64:
        raise argparse.ArgumentTypeError("seed must fit in an unsigned 64-bit integer")
    return seed


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", type=Path, default=DEFAULT_SUITE)
    parser.add_argument("--case", action="append", dest="selected_cases")
    parser.add_argument("--repetitions", type=int, default=DEFAULT_REPETITIONS)
    parser.add_argument("--seed", type=parse_seed)
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--dry-run", action="store_true", help="calibrate cases without model calls")
    parser.add_argument("--bettercodex", type=Path)
    parser.add_argument("--codex", type=Path, default=Path(shutil.which("codex") or "codex"))
    parser.add_argument("--auth", type=Path)
    parser.add_argument("--timeout-seconds", type=int, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--require-pass", action="store_true")
    return parser


def main() -> int:
    parser = build_parser()
    arguments = parser.parse_args()
    if arguments.repetitions < 1:
        parser.error("--repetitions must be positive")
    if arguments.timeout_seconds < 1:
        parser.error("--timeout-seconds must be positive")
    try:
        suite = load_suite(arguments.suite)
        cases = select_cases(suite, arguments.selected_cases)
    except RuntimeError as error:
        parser.error(str(error))
    if arguments.list:
        for case in cases:
            print(
                f"{case.name}\t{case.family}\tcritical={str(case.critical).lower()}\t"
                f"provenance={case.provenance}\ttags={','.join(case.tags)}"
            )
        return 0

    seed = arguments.seed if arguments.seed is not None else secrets.randbits(64)
    print(
        f"Calibrating {len(cases)} case(s) at seed {seed}",
        file=sys.stderr,
        flush=True,
    )
    try:
        calibrations = calibrations_for(cases, arguments.repetitions, seed)
    except RuntimeError as error:
        parser.error(str(error))
    if arguments.dry_run:
        print(json.dumps({"seed": seed, "calibrations": calibrations}, indent=2))
        return 0
    if arguments.bettercodex is None or arguments.output is None:
        parser.error("live runs require --bettercodex and --output")

    try:
        auth = source_auth_path(arguments.auth)
        redactor = SecretRedactor(auth_values(auth))
        arms = {
            "bettercodex": make_arm("bettercodex", "bettercodex", arguments.bettercodex),
            "codex": make_arm("codex", "codex", arguments.codex),
        }
    except RuntimeError as error:
        parser.error(str(error))

    schedule = shuffled_schedule(cases, arguments.repetitions, seed)
    case_by_name = {case.name: case for case in cases}
    document: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "status": "running",
        "started_at": datetime.now(UTC).isoformat(),
        "protocol": {
            "runner_sha256": sha256_file(Path(__file__)),
            "model": MODEL,
            "reasoning_effort": REASONING_EFFORT,
            "seed": seed,
            "repetitions": arguments.repetitions,
            "timeout_seconds": arguments.timeout_seconds,
            "schedule": [
                {
                    "case": case_name,
                    "repetition": repetition,
                    "arm_order": arm_order(seed, case_name, repetition),
                }
                for case_name, repetition in schedule
            ],
            "acceptance": {
                "minimum_repetitions": MINIMUM_RELEASE_REPETITIONS,
                "no_lower_total_passes": True,
                "no_lower_passes_in_any_case": True,
                "every_critical_repetition_must_pass": True,
                "all_protocol_records_must_be_complete": True,
                "synthetic_cases_are_never_release_eligible": True,
            },
        },
        "suite": {
            "name": suite.name,
            "classification": suite.classification,
            "declared_release_eligible": suite.declared_release_eligible,
            "required_repetitions": suite.required_repetitions,
            "corpus_sha256": suite.corpus_sha256,
            "selected_cases": [case.name for case in cases],
            "release_corpus_checks": release_corpus_checks(suite),
        },
        "arms": {
            name: {
                "kind": arm.kind,
                "binary": os.fspath(arm.binary),
                "binary_sha256": arm.binary_sha256,
                "version": arm.version,
            }
            for name, arm in arms.items()
        },
        "calibrations": calibrations,
        "results": [],
    }
    output = arguments.output.expanduser().resolve()
    atomic_write_json(output, document)
    total = len(schedule) * len(arms)
    completed = 0
    for case_name, repetition in schedule:
        case = case_by_name[case_name]
        scenario_seed, nonce = derive_scenario(seed, case.name, repetition)
        prompt = render_template(case.prompt, nonce=nonce, scenario_seed=scenario_seed)
        for arm_name in arm_order(seed, case_name, repetition):
            completed += 1
            print(
                f"[{completed}/{total}] {case.name} repetition {repetition} {arm_name}",
                flush=True,
            )
            result = run_arm(
                arms[arm_name],
                case,
                prompt=prompt,
                nonce=nonce,
                scenario_seed=scenario_seed,
                auth=auth,
                redactor=redactor,
                timeout_seconds=arguments.timeout_seconds,
            )
            result["repetition"] = repetition
            result["nonce"] = nonce
            result["scenario_seed"] = scenario_seed
            document["results"].append(result)
            atomic_write_json(output, document)

    summary = summarize_results(document["results"], cases, arguments.repetitions, suite)
    document["status"] = "complete"
    document["completed_at"] = datetime.now(UTC).isoformat()
    document["summary"] = summary
    atomic_write_json(output, document)
    print(json.dumps(summary, indent=2))
    print(f"Complete results: {output}")
    if arguments.require_pass and summary["release"]["accepted"] is not True:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
