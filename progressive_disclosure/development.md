# bettercodex development workflows

Read this before changing Rust, tests, performance, or installation behavior.
bettercodex follows Codex's Cargo and `just` workflow, reduced to the one
retained Rust package. It does not carry Codex's Bazel, Node, or unrelated
release-packaging machinery. Native Windows support follows the same one-package
design and keeps platform dependencies and code target-gated.

## Setup and daily commands

Install the tools used by the upstream workflow:

```sh
rustup component add rustfmt clippy
cargo install --locked just
cargo install --locked cargo-nextest
cargo_tools_dir="${CARGO_HOME:-$HOME/.cargo}/bin"
export PATH="$cargo_tools_dir:$PATH"
command -v just
cargo nextest --version
just install
```

Cargo can install a command successfully while leaving it unavailable to the
current shell. The explicit PATH setup and version preflight above catch that
case against the pinned toolchain before a long validation starts.

Build or run the binary through the checked-in Cargo wrapper:

```sh
./scripts/cargo-with-v8.sh build
./scripts/cargo-with-v8.sh run --bin bcodex -- "explain this codebase"
```

In PowerShell on Windows, use the equivalent checked-in wrapper:

```powershell
.\scripts\cargo-with-v8.ps1 build
.\scripts\cargo-with-v8.ps1 run --bin bcodex -- "explain this codebase"
```

The wrapper downloads and verifies the sandbox-enabled V8 archive and generated
binding published by upstream Codex, then delegates every argument to Cargo.
Use it for every command that builds or checks bettercodex, including `check`,
`test`, and `clippy`. Raw Cargo does not set the pinned V8 archive and binding;
for debug profiles it can request an archive that upstream does not publish and
fail with HTTP 404. Formatting and dependency-only commands can invoke Cargo
directly:

```sh
./scripts/cargo-with-v8.sh check --tests
```

Use the matching upstream-style recipes for routine development:

```sh
just fmt
just fix
just test
just clippy -- -D warnings
```

`just test <filter>` forwards filters and other arguments to Cargo Nextest.
Do not add a wrapper for a command that Cargo or the retained `justfile`
already expresses.

## Rust design

- Keep modules private unless another module needs an explicit export.
- Avoid boolean and ambiguous `Option` parameters when a named method, enum,
  or small type makes the call readable.
- Prefer direct code over a generic abstraction or helper with one caller.
- Prefer exhaustive `match` arms when variants are known.
- Let rustfmt and Clippy enforce mechanical style.

## Tests

- Test affected behavior rather than implementation details or static values.
- Keep tests ported with retained upstream behavior.
- Add bettercodex-only tests only for a deliberate product departure or a
  regression that cannot be covered by an existing test.
- Use sibling `*_tests.rs` modules for new test modules.
- Do not put manual benchmarks behind `#[test]`; add a real benchmark target
  only when a repeated performance workflow justifies one.
- Prefer equality of complete values over many field assertions.

This package has one binary target and no library target. For filtered tests,
use `just test <filter>`, `cargo test <filter>`, or an explicit binary selector;
`cargo test --lib` always fails before running a test:

```sh
RUST_MIN_STACK=8388608 ./scripts/cargo-with-v8.sh test --bin bcodex <filter>
```

If Cargo Nextest is not installed, the command above is the supported fallback
for both filtered and full validation. Do not assume that a previous shell's
Nextest installation is visible; run `cargo nextest --version` first.

## Shared checkout and artifact ownership

The primary checkout is shared integration state and stays on `main`. Give
concurrent editing sessions separate linked worktrees and branches so they do
not share an index or move the primary checkout's `HEAD`. Immediately before a
commit, inspect `git diff --cached` and `git status`; serialize final integration
into `main` so the staged tree, commit message, and validated revision cannot be
mixed with another session.

Routine commands may reuse the checkout's warm `target/`. Cargo's own lock
coordinates writers; never delete that directory as cleanup. An isolated
worktree or benchmark that needs a separate target must use a unique,
session-owned directory on a disk-backed filesystem. A clean target needs about
6 GB, so first inspect both free space and existing targets:

```sh
df -h . "${XDG_CACHE_HOME:-$HOME/.cache}"
du -sh target 2>/dev/null || true
validation_parent="${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex/validation"
mkdir -p "$validation_parent"
validation_root="$(mktemp -d "$validation_parent/session.XXXXXXXX")"
export CARGO_TARGET_DIR="$validation_root/target"
```

Do not put an isolated target on `/tmp` unless its backing filesystem has ample
capacity; on common Linux hosts `/tmp` is a small tmpfs. Keep the generated root
private to its owning session. Cleanup jobs must not scan by directory name or
age, and the owner must wait for its Cargo command to finish before removing it.
Clean the owned root on success and failure:

```sh
cleanup_validation() {
    cargo clean --target-dir "$CARGO_TARGET_DIR" >/dev/null 2>&1 || true
    rm -rf -- "$validation_root"
}
trap cleanup_validation EXIT HUP INT TERM
```

Never share one target across worktrees for an A/B benchmark without rebuilding
the package itself: older source mtimes can make Cargo reuse the wrong binary.
Prefer distinct targets. If one session owns a shared benchmark target, run
`cargo clean --target-dir "$CARGO_TARGET_DIR" -p bettercodex` between worktrees
while retaining dependency artifacts.

## Portable diagnostics and benchmarks

Install `hyperfine` for repeatable wall-time measurements and the platform's
system-call tracer (`strace` on Linux) before startup-performance work. Record a
release baseline before changing the hot path:

```sh
cargo install --locked hyperfine
./scripts/cargo-with-v8.sh build --release --locked
hyperfine --warmup 3 --runs 20 './target/release/bcodex --version'
strace -f -c ./target/release/bcodex --version  # Linux
```

For a one-off Bash measurement, use the reserved word and `TIMEFORMAT`.
`command -v time` is not an external-binary preflight: Bash can report its
keyword even when GNU time is absent. Invoke GNU-specific metrics only after an
executable check:

```sh
TIMEFORMAT='%3R real %3U user %3S sys'
time ./target/release/bcodex --version
if test -x /usr/bin/time; then
    /usr/bin/time -v ./target/release/bcodex --version
fi
```

Prefer workload-internal `Instant` measurements for focused hot paths. Keep
repeatable benchmarks out of ignored tests, and record the command, revision,
warmup count, run count, and machine with every comparison.

## Host-safe probes and repository scoping

This checkout has both `origin` and `upstream`; pin GitHub CLI operations to the
bettercodex repository instead of relying on remote inference:

```sh
gh pr list --repo ummay0432/bettercodex
gh repo view ummay0432/bettercodex
```

Run `cargo info` and similar dependency-only probes outside this manifest so
repository patches do not fetch every Git dependency. A disposable Cargo home
also prevents inspection from churning the development cache:

```sh
probe_root="$(mktemp -d)"
(cd "$probe_root" && CARGO_HOME="$probe_root/cargo-home" cargo info <crate>)
rm -rf -- "$probe_root"
```

Do not assume `nc -U` or `socat` exists for Unix-socket smoke tests. Use a
repository test helper or a standard-library client, for example:

```sh
python3 - /absolute/path/to/socket <<'PY'
import socket
import sys

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
    client.connect(sys.argv[1])
    client.sendall(b"ping\n")
    print(client.recv(4096).decode(), end="")
PY
```

## Finish a change

Before integration, run:

```sh
just fix
just fmt
just test
just clippy -- -D warnings
python3 scripts/install_tests.py
./scripts/cargo-with-v8.sh build --release --locked
```

Cargo can wait on a shared cache or build lock. Let it finish; do not kill a
Cargo or Rust process by PID to make the lock disappear.
