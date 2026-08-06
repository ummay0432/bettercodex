#!/usr/bin/env python3
"""Small, dependency-free developer workflows for bettercodex."""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import re
import resource
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


REPOSITORY = Path(__file__).resolve().parent.parent
TOOL_CONTEXT = REPOSITORY / "prompts" / "tool-context.md"
TIKTOKEN_VERSION = "0.11.0"
LOW_SPACE_BYTES = 15 * 1024**3
TEMP_CANDIDATE_AGE_SECONDS = 6 * 60 * 60
MAX_REPORTED_TEMP_CANDIDATES = 20
CANONICAL_BRANCH = "refs/heads/main"
CANONICAL_REMOTE_BRANCH = "refs/remotes/origin/main"
INSTALL_LOCK_FILE = ".bettercodex-install.lock"
MAX_INSTALL_ATTEMPTS = 3


def git(*arguments: str, cwd: Path = REPOSITORY, check: bool = True) -> str:
    result = subprocess.run(
        ["git", *arguments],
        cwd=cwd,
        check=check,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result.stdout.strip()


def resolved_git_commit(reference: str, cwd: Path) -> str | None:
    result = subprocess.run(
        ["git", "rev-parse", "--verify", f"{reference}^{{commit}}"],
        cwd=cwd,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        return None
    return result.stdout.strip()


def is_ancestor(ancestor: str, descendant: str, cwd: Path) -> bool:
    return (
        subprocess.run(
            ["git", "merge-base", "--is-ancestor", ancestor, descendant],
            cwd=cwd,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        ).returncode
        == 0
    )


def repository_root() -> Path:
    return Path(git("rev-parse", "--show-toplevel")).resolve()


def main_worktree_root() -> Path:
    output = git("worktree", "list", "--porcelain")
    first = next(
        (line.removeprefix("worktree ") for line in output.splitlines() if line.startswith("worktree ")),
        None,
    )
    if first is None:
        raise RuntimeError("git worktree list returned no worktrees")
    return Path(first).resolve()


def worktree_target(worktree: Path | None = None) -> Path:
    worktree = (worktree or repository_root()).resolve()
    main = main_worktree_root()
    if worktree == main:
        return main / "target"
    slug = re.sub(r"[^A-Za-z0-9._-]+", "-", worktree.name).strip("-") or "worktree"
    digest = hashlib.sha256(os.fsencode(worktree)).hexdigest()[:12]
    return main / "target" / "worktrees" / f"{slug}-{digest}"


def cargo_environment() -> tuple[dict[str, str], Path]:
    target = worktree_target()
    target.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = os.fspath(target)
    return environment, target


def human_bytes(value: int) -> str:
    amount = float(value)
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if amount < 1024 or unit == "TiB":
            return f"{amount:.1f} {unit}"
        amount /= 1024
    raise AssertionError("unreachable")


def path_size(path: Path) -> int:
    if not path.exists():
        return 0
    result = subprocess.run(
        ["du", "-sk", os.fspath(path)],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    return int(result.stdout.split(maxsplit=1)[0]) * 1024


def active_worktrees() -> set[Path]:
    return {
        Path(line.removeprefix("worktree ")).resolve()
        for line in git("worktree", "list", "--porcelain").splitlines()
        if line.startswith("worktree ")
    }


def expected_worktree_targets() -> set[Path]:
    return {worktree_target(worktree) for worktree in active_worktrees()}


def stale_worktree_targets() -> list[Path]:
    root = main_worktree_root() / "target" / "worktrees"
    expected = expected_worktree_targets()
    if not root.is_dir():
        return []
    return sorted(path for path in root.iterdir() if path.is_dir() and path not in expected)


def stale_temp_candidates() -> list[Path]:
    now = time.time()
    active = active_worktrees()
    candidates: list[Path] = []
    for pattern in ("bettercodex-*", "bcodex-*"):
        for path in Path(tempfile.gettempdir()).glob(pattern):
            try:
                age = now - path.stat().st_mtime
            except FileNotFoundError:
                continue
            if path.resolve() not in active and age >= TEMP_CANDIDATE_AGE_SECONDS:
                candidates.append(path)
    return sorted(set(candidates))


def has_broad_origin_fetch() -> bool:
    result = subprocess.run(
        ["git", "config", "--get-all", "remote.origin.fetch"],
        cwd=REPOSITORY,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
    )
    return any(
        "refs/heads/*:refs/remotes/origin/*" in line for line in result.stdout.splitlines()
    )


def canonical_main_commit(repository: Path) -> str:
    commit = resolved_git_commit(CANONICAL_BRANCH, repository)
    if commit is None:
        raise RuntimeError("cannot install bettercodex because the local main branch is missing")
    remote_commit = resolved_git_commit(CANONICAL_REMOTE_BRANCH, repository)
    if remote_commit is not None and not is_ancestor(remote_commit, commit, repository):
        raise RuntimeError(
            "local main does not contain origin/main; update main before installing bettercodex"
        )
    return commit


def validate_install_caller(worktree: Path, main_commit: str) -> None:
    status = git("status", "--porcelain=v1", "--untracked-files=all", cwd=worktree)
    if status:
        raise RuntimeError(
            "the invoking worktree is dirty; commit its work before installing canonical main"
        )
    head = git("rev-parse", "HEAD", cwd=worktree)
    if not is_ancestor(head, main_commit, worktree):
        raise RuntimeError(
            "the invoking worktree's HEAD is not integrated into local main; merge it before installing"
        )


@contextlib.contextmanager
def install_lock(install_root: Path):
    install_root.mkdir(parents=True, exist_ok=True)
    lock_path = install_root / INSTALL_LOCK_FILE
    with lock_path.open("a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)


@contextlib.contextmanager
def committed_source_snapshot(repository: Path, commit: str):
    with tempfile.TemporaryDirectory(prefix=f"bettercodex-install-{commit[:12]}-") as temporary:
        root = Path(temporary)
        archive = root / "source.tar"
        source = root / "source"
        source.mkdir()
        subprocess.run(
            ["git", "archive", "--format=tar", "--output", os.fspath(archive), commit],
            cwd=repository,
            check=True,
        )
        subprocess.run(
            ["tar", "-xf", os.fspath(archive), "-C", os.fspath(source)],
            check=True,
        )
        yield source


def command_install(arguments: argparse.Namespace) -> int:
    worktree = repository_root()
    primary = main_worktree_root()
    install_root = arguments.root.expanduser().resolve()
    target = primary / "target" / "canonical-install"
    target.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = os.fspath(target)

    with install_lock(install_root):
        for attempt in range(1, MAX_INSTALL_ATTEMPTS + 1):
            commit = canonical_main_commit(primary)
            validate_install_caller(worktree, commit)
            print(f"Installing committed main {commit[:12]} into {install_root}")
            with committed_source_snapshot(primary, commit) as source:
                subprocess.run(
                    [
                        "cargo",
                        "install",
                        "--locked",
                        "--path",
                        os.fspath(source),
                        "--force",
                        "--root",
                        os.fspath(install_root),
                    ],
                    cwd=source,
                    env=environment,
                    check=True,
                )

            latest = canonical_main_commit(primary)
            if latest == commit:
                binary = install_root / "bin" / "bcodex"
                subprocess.run([binary, "--version"], check=True)
                print(f"Installed canonical bettercodex {commit[:12]} at {binary}")
                return 0
            print(
                f"main advanced from {commit[:12]} to {latest[:12]} during installation; retrying",
                file=sys.stderr,
            )

    raise RuntimeError(
        f"main changed during all {MAX_INSTALL_ATTEMPTS} installation attempts; retry once it settles"
    )


def command_preflight(_: argparse.Namespace) -> int:
    root = repository_root()
    target = worktree_target(root)
    target.mkdir(parents=True, exist_ok=True)
    usage = shutil.disk_usage(target)
    print(f"Worktree:    {root}")
    print(f"Cargo target: {target}")
    print(f"Filesystem:   {human_bytes(usage.free)} free of {human_bytes(usage.total)}")
    if usage.free < LOW_SPACE_BYTES:
        print(
            f"WARNING: less than {human_bytes(LOW_SPACE_BYTES)} is free; a clean bettercodex build can be large.",
            file=sys.stderr,
        )

    main_target = main_worktree_root() / "target"
    if main_target.exists():
        print(f"Main target:  {human_bytes(path_size(main_target))} at {main_target}")

    stale_targets = stale_worktree_targets()
    print("\nInactive per-worktree Cargo targets:")
    if stale_targets:
        for path in stale_targets:
            print(f"  {human_bytes(path_size(path)):>10}  {path}")
    else:
        print("  none")

    temp_candidates = stale_temp_candidates()
    print(f"\nTemporary candidates older than {TEMP_CANDIDATE_AGE_SECONDS // 3600} hours:")
    if temp_candidates:
        sized_candidates = sorted(
            ((path_size(path), path) for path in temp_candidates), reverse=True
        )
        for size, path in sized_candidates[:MAX_REPORTED_TEMP_CANDIDATES]:
            print(f"  {human_bytes(size):>10}  {path}")
        omitted = len(sized_candidates) - MAX_REPORTED_TEMP_CANDIDATES
        if omitted > 0:
            print(
                f"  … {omitted} smaller candidate(s) omitted; "
                f"{human_bytes(sum(size for size, _ in sized_candidates))} total"
            )
    else:
        print("  none")

    if not has_broad_origin_fetch():
        print(
            "\nWARNING: origin fetches only selected branches. Repair it with:\n"
            "  git config remote.origin.fetch '+refs/heads/*:refs/remotes/origin/*'\n"
            "  git fetch --prune origin",
            file=sys.stderr,
        )
    return 0


def command_cargo(arguments: argparse.Namespace) -> int:
    environment, target = cargo_environment()
    free = shutil.disk_usage(target).free
    if free < LOW_SPACE_BYTES:
        print(
            f"warning: only {human_bytes(free)} is free; run ./scripts/dev.py preflight before a large build",
            file=sys.stderr,
        )
    print(f"CARGO_TARGET_DIR={target}", file=sys.stderr)
    os.execvpe("cargo", ["cargo", *arguments.cargo_arguments], environment)
    raise AssertionError("exec returned")


def measured_process(command: list[str]) -> tuple[float, int]:
    started = time.perf_counter()
    child = os.fork()
    if child == 0:
        try:
            null = os.open(os.devnull, os.O_RDWR)
            os.dup2(null, 0)
            os.dup2(null, 1)
            os.dup2(null, 2)
            os.execvp(command[0], command)
        except BaseException:
            os._exit(127)
    _, status, usage = os.wait4(child, 0)
    elapsed = time.perf_counter() - started
    exit_code = os.waitstatus_to_exitcode(status)
    if exit_code != 0:
        raise RuntimeError(f"benchmark command exited with status {exit_code}: {command!r}")
    peak_rss = int(usage.ru_maxrss)
    if sys.platform != "darwin":
        peak_rss *= 1024
    return elapsed, peak_rss


def default_benchmark_command() -> list[str]:
    installed = Path.home() / ".local" / "bin" / "bcodex"
    binary = installed if installed.exists() else worktree_target() / "release" / "bcodex"
    if not binary.exists():
        raise RuntimeError(
            f"no benchmark binary at {binary}; install bettercodex or build it with ./scripts/dev.py cargo build --release"
        )
    return [os.fspath(binary), "--version"]


def command_benchmark(arguments: argparse.Namespace) -> int:
    command = arguments.command or default_benchmark_command()
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        raise RuntimeError("benchmark command cannot be empty")
    for _ in range(arguments.warmups):
        measured_process(command)
    measurements = [measured_process(command) for _ in range(arguments.runs)]
    elapsed = [measurement[0] for measurement in measurements]
    rss = [measurement[1] for measurement in measurements]
    print(f"Command:     {shlex.join(command)}")
    print(f"Runs:        {arguments.runs} ({arguments.warmups} warmup)")
    print(f"Elapsed:     median {statistics.median(elapsed) * 1000:.2f} ms")
    print(f"             min {min(elapsed) * 1000:.2f} ms, max {max(elapsed) * 1000:.2f} ms")
    print(f"Peak RSS:    max {human_bytes(max(rss))}")
    return 0


def import_tiktoken() -> Any:
    try:
        import tiktoken

        return tiktoken
    except ModuleNotFoundError:
        if os.environ.get("BCODEX_TIKTOKEN_BOOTSTRAP") == "1":
            raise RuntimeError(
                f"uv did not provide tiktoken {TIKTOKEN_VERSION} to the audit process"
            ) from None
        uv = shutil.which("uv")
        if uv is None:
            raise RuntimeError(
                f"tool-context needs tiktoken {TIKTOKEN_VERSION}; install it or install uv so the script can run it ephemerally"
            ) from None
        environment = os.environ.copy()
        environment["BCODEX_TIKTOKEN_BOOTSTRAP"] = "1"
        os.execvpe(
            uv,
            [
                uv,
                "run",
                "--quiet",
                "--with",
                f"tiktoken=={TIKTOKEN_VERSION}",
                "--",
                "python",
                os.fspath(Path(__file__).resolve()),
                *sys.argv[1:],
            ],
            environment,
        )
        raise AssertionError("exec returned")


def compact_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


@dataclass(frozen=True)
class Metrics:
    byte_count: int
    token_count: int

    @property
    def bytes_per_four(self) -> int:
        return (self.byte_count + 3) // 4


def metrics(text: str, encoding: Any) -> Metrics:
    return Metrics(len(text.encode()), len(encoding.encode(text)))


def metric_table(rows: list[tuple[str, Metrics]]) -> str:
    output = [
        "| Injected component | UTF-8 bytes | o200k | bytes/4 |",
        "| --- | ---: | ---: | ---: |",
    ]
    output.extend(
        f"| {label} | {value.byte_count:,} | {value.token_count:,} | {value.bytes_per_four:,} |"
        for label, value in rows
    )
    return "\n".join(output)


def exec_section_table(description: str, encoding: Any) -> str:
    headings = [
        ("`apply_patch`", "### `apply_patch`"),
        ("`exec_command`", "### `exec_command`"),
        ("`log_papercut`", "### `log_papercut`"),
        ("`update_plan`", "### `update_plan`"),
        ("`view_image`", "### `view_image`"),
        ("`write_stdin`", "### `write_stdin`"),
        ("`web` namespace and `web__run`", "## web"),
    ]
    starts = [description.index(heading) for _, heading in headings]
    chunks = [description[: starts[0] - 1]]
    for index, start in enumerate(starts):
        end = starts[index + 1] if index + 1 < len(starts) else len(description)
        chunk = description[start:end]
        if index + 1 < len(starts) and chunk.endswith("\n\n"):
            chunk = chunk[:-1]
        chunks.append(chunk)
    rows = [("Runtime rules and global helpers", metrics(chunks[0], encoding))]
    rows.extend(
        (label, metrics(chunk, encoding))
        for (label, _), chunk in zip(headings, chunks[1:], strict=True)
    )
    return metric_table(rows).replace("Injected component", "Section inside `exec`")


def render_audit() -> tuple[str, str, str]:
    tiktoken = import_tiktoken()
    encoding = tiktoken.get_encoding("o200k_base")
    environment, _ = cargo_environment()
    result = subprocess.run(
        ["cargo", "run", "--quiet", "--locked", "--", "--tool-context-json"],
        cwd=REPOSITORY,
        env=environment,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    audit = json.loads(result.stdout)
    instructions = audit["instructions"]
    stable = audit["stable_prefix"]
    [additional_tools] = stable
    exec_specification, wait_specification = additional_tools["tools"]
    exec_description = exec_specification["description"]
    stable_rows = [
        (
            "Complete stable harness input: `instructions` plus `additional_tools`",
            metrics(
                compact_json({"instructions": instructions, "input": stable}),
                encoding,
            ),
        ),
        ("Complete `additional_tools` developer item", metrics(compact_json(additional_tools), encoding)),
        ("Top-level `exec` specification", metrics(compact_json(exec_specification), encoding)),
        ("`exec` description only", metrics(exec_description, encoding)),
        ("`exec` Lark grammar only", metrics(exec_specification["format"]["definition"], encoding)),
        ("Top-level `wait` specification", metrics(compact_json(wait_specification), encoding)),
        ("`wait` description only", metrics(wait_specification["description"], encoding)),
        (
            "Top-level `instructions` request field",
            metrics(compact_json({"instructions": instructions}), encoding),
        ),
        ("`prompts/system.md` text only", metrics(instructions, encoding)),
    ]

    dynamic_rows = []
    for item in audit["world_state"]:
        text = item["content"][0]["text"]
        if text.startswith("<environment_context>"):
            label = "Current `<environment_context>` developer item"
        elif text.startswith("<repository_context>"):
            label = "Current `<repository_context>` user item"
        elif text.startswith("<available_skills>"):
            label = "Current `<available_skills>` user item"
        else:
            raise RuntimeError("tool-context audit returned an unknown world-state item")
        dynamic_rows.append((label, metrics(compact_json(item), encoding)))

    return (
        metric_table(stable_rows),
        exec_section_table(exec_description, encoding),
        metric_table(dynamic_rows).replace("Injected component", "Dynamic message item"),
    )


def generated_region(document: str, name: str) -> tuple[int, int, str]:
    start_marker = f"<!-- bcodex-tool-context:{name}:start -->"
    end_marker = f"<!-- bcodex-tool-context:{name}:end -->"
    start = document.index(start_marker) + len(start_marker)
    end = document.index(end_marker, start)
    return start, end, document[start:end].strip()


def replace_generated_region(document: str, name: str, replacement: str) -> str:
    start, end, _ = generated_region(document, name)
    return document[:start] + f"\n{replacement}\n" + document[end:]


def command_tool_context(arguments: argparse.Namespace) -> int:
    stable, sections, dynamic = render_audit()
    rendered = {"stable": stable, "sections": sections, "dynamic": dynamic}
    document = TOOL_CONTEXT.read_text()
    if arguments.update:
        for name, table in rendered.items():
            document = replace_generated_region(document, name, table)
        TOOL_CONTEXT.write_text(document)
        print(f"updated {TOOL_CONTEXT.relative_to(REPOSITORY)}")
        return 0

    mismatches = []
    for name in ("stable", "sections"):
        _, _, existing = generated_region(document, name)
        if existing != rendered[name]:
            mismatches.append(name)
    if mismatches:
        print(
            "tool-context snapshot drifted in "
            + ", ".join(mismatches)
            + "; run ./scripts/dev.py tool-context --update",
            file=sys.stderr,
        )
        return 1
    print("stable tool-context snapshot matches the rendered request")
    print("\nCurrent dynamic world-state metrics (informational):\n")
    print(dynamic)
    return 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="subcommand", required=True)

    cargo = commands.add_parser("cargo", help="run Cargo with a safe per-worktree target")
    cargo.add_argument("cargo_arguments", nargs=argparse.REMAINDER)
    cargo.set_defaults(function=command_cargo)

    install = commands.add_parser(
        "install", help="install a serialized snapshot of committed local main"
    )
    install.add_argument(
        "--root",
        type=Path,
        default=Path.home() / ".local",
        help="Cargo installation root (default: ~/.local)",
    )
    install.set_defaults(function=command_install)

    preflight = commands.add_parser(
        "preflight", help="show disk, stale target/temp, and Git fetch-refspec risks"
    )
    preflight.set_defaults(function=command_preflight)

    benchmark = commands.add_parser(
        "benchmark", help="measure startup elapsed time and peak RSS without /usr/bin/time"
    )
    benchmark.add_argument("--runs", type=int, default=5)
    benchmark.add_argument("--warmups", type=int, default=1)
    benchmark.add_argument("command", nargs=argparse.REMAINDER)
    benchmark.set_defaults(function=command_benchmark)

    tool_context = commands.add_parser(
        "tool-context", help="verify or update prompts/tool-context.md from rendered items"
    )
    mode = tool_context.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="verify stable snapshot rows (default)")
    mode.add_argument("--update", action="store_true", help="rewrite all snapshot tables")
    tool_context.set_defaults(function=command_tool_context)
    return root


def main() -> int:
    arguments = parser().parse_args()
    if getattr(arguments, "runs", 1) < 1 or getattr(arguments, "warmups", 0) < 0:
        raise RuntimeError("benchmark runs must be positive and warmups cannot be negative")
    return arguments.function(arguments)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
