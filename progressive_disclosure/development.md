# bettercodex development workflows

Read this before changing Rust, build scripts, tests, performance, or
installation behavior.

Run developer workflows through the checked-in helper:

```sh
./scripts/dev.py preflight
./scripts/dev.py cargo test
```

## Rust design

- Keep modules private unless another module needs an explicit export. Keep the
  exported API as small as the current program permits.
- Avoid boolean and ambiguous `Option` parameters that produce calls such as
  `run(false)` or `open(None)`. Use a named method, enum, or small type when that
  makes the call readable. Prefer exhaustive `match` arms when the variants are
  known.
- Prefer direct code over a generic abstraction or helper with one caller. When
  a large orchestration file needs new coherent behavior, put that behavior in
  a focused module and move its tests and type documentation with it.
- Let `rustfmt` and Clippy enforce mechanical style.

## Tests

- Test affected behavior, not only an extracted helper.
- When adding a new test module, use a sibling `*_tests.rs` file and an explicit
  `#[path = "..."]`. Do not move an existing inline test module only to follow
  this convention.
- Prefer equality of complete values over a series of field assertions. Avoid
  test-only functions in production code and avoid mutating process environment
  variables in tests.
- Inference and terminal changes have additional test requirements in
  `inference.md` and `terminal-ui.md`.

## Finish a code change

The primary checkout is shared integration state and must stay on `main`.
Before editing, create a dedicated linked worktree and feature branch for the
session. Run validation and commit there so another session cannot move the
checkout's `HEAD` or alter the tested tree. If the primary checkout is dirty,
leave it untouched; another session owns those changes.

Every change must pass the repository text check:

```sh
./scripts/check-no-emoji.sh
```

Every Rust change must pass all three commands; treat every Clippy warning as an
error:

```sh
./scripts/dev.py cargo fmt --all
./scripts/dev.py cargo test
./scripts/dev.py cargo clippy --all-targets -- -D warnings
```

Changes to `scripts/dev.py` must also pass its dependency-free unit tests:

```sh
python3 -m unittest -v scripts.dev_tests
```

Changes to the private source installer or its local-build contract must pass:

```sh
python3 -m unittest -v scripts.install_tests
```

The installer resolves integrated `main` (or an explicitly requested stable
tag) to an immutable commit and builds its source through `./scripts/dev.py
package-build`. It passes the exact 40-digit commit in
`BCODEX_SOURCE_REVISION`; keep that command usable from a source archive without
a Git checkout. Bettercodex does not publish prebuilt binaries or use hosted
release builds.

Changes to the matched live-evaluation runner must pass its offline protocol
tests as well:

```sh
python3 -m unittest -v scripts.evaluate_harness_tests
```

Cargo can wait on a shared build lock. Let it finish; do not kill a Cargo or
Rust process by PID to make the lock disappear.

Feature worktrees must smoke-test their isolated release binary before
integration. Build it with `./scripts/dev.py cargo build --release`; the helper
prints the target directory containing `release/bcodex`. Run that binary with
`--internal-package-smoke` when package/runtime behavior changed.

After committing and integrating the work into local `main`, install the
canonical binary:

```sh
./scripts/dev.py install
```

Then run the relevant final smoke test against `$HOME/.local/bin/bcodex`.

Never run `cargo install` directly from a worktree. The global binary is for
committed local `main`; installing a feature worktree can silently replace it
with a branch that predates already-integrated fixes. The install helper rejects
dirty or unmerged callers, serializes concurrent installs, archives committed
`main` so unrelated shared-worktree edits cannot leak into the build, and
retries if `main` advances while Cargo is running.

## Cargo worktrees and disk space

`./scripts/dev.py cargo …` gives every linked Git worktree a distinct Cargo
target under the primary worktree's `target/worktrees/` directory. This avoids
executing a package artifact produced from another concurrently changing
worktree, while keeping large build output on the primary worktree's filesystem
instead of a space-constrained `/tmp` mount. The primary worktree continues to
use its normal `target/` directory.

The helper also downloads and SHA-256 verifies OpenAI's pinned, sandbox-enabled
V8 archive and matching Rust bindings for the current Linux or macOS target.
The crates.io release does not publish that feature combination, so direct
Cargo builds need explicit `RUSTY_V8_ARCHIVE` and
`RUSTY_V8_SRC_BINDING_PATH` overrides; use the helper for normal workflows.

Do not point `CARGO_TARGET_DIR` at another active worktree. Use the helper for
focused tests and for the complete validation sequence above.

`./scripts/dev.py preflight` reports free space, the selected target, inactive
per-worktree target directories, bettercodex test/temp directories older than
six hours, and a narrow `origin` fetch refspec. It never deletes anything.
Review the reported paths and remove only inactive targets or expired temp
directories. `cargo clean --target-dir /reported/target` is the safe Cargo
cleanup command; never clean a target used by a running Cargo process.

## Startup measurements

The portable startup helper uses `wait4(2)` and does not require GNU
`/usr/bin/time`. By default it measures the installed binary's `--version`
path (or the current release target when no installed binary exists):

```sh
./scripts/dev.py benchmark --runs 10
./scripts/dev.py benchmark --runs 10 -- "$HOME/.local/bin/bcodex" --help
```

It reports median/min/max elapsed time and maximum child RSS. The RSS figure
includes the small Python fork floor, so compare runs made with the same helper
and host rather than treating it as an exact allocator measurement.

## Managed terminal sessions

Automatic tmux management is enabled by default and can be changed from the TUI
with `/tmux` or directly with `/tmux on|off`. When enabled, interactive agent
invocations outside tmux create and attach to the first free `c1`, `c2`, …
session. The pane is dedicated to the bettercodex process lifecycle:
disconnecting a client leaves it running, while exiting bettercodex destroys the
session and releases its name. Toggling the setting affects the next launch and
does not move or close the current session. When disabled, startup does not
invoke tmux at all. Automatic management requires tmux on both Linux and macOS
when enabled. The invoking environment crosses an existing tmux server through a
private one-use file; never put its values back into tmux command-line arguments.

On macOS, every agent invocation re-executes once through
`/usr/bin/caffeinate -i -s`. The wrapper is process-scoped and exits with
bettercodex; diagnostics such as `--help` and `--version` do not acquire a sleep
assertion.

## Tool-context audit

The audit command builds the current worktree, asks `bcodex` to render the real
stable request prefix and dynamic world-state items, and calculates both
bytes/4 and pinned `tiktoken` `o200k_base` estimates:

```sh
./scripts/dev.py tool-context --check
./scripts/dev.py tool-context --update
```

`--check` verifies the deterministic stable-prefix and exec-section tables in
`prompts/tool-context.md` and prints current machine-dependent world-state
metrics. `--update` rewrites all tables. If Python cannot import the pinned
`tiktoken` version, the helper uses an installed `uv` to provide it ephemerally.

## Remote branch publication and verification

Publish branches with an explicit source and destination refspec:

```sh
git push origin HEAD:refs/heads/<branch>
```

Do not rely on plain `git push`: a clone-level `remote.origin.push` setting can
silently redirect it to `main`. Immediately before integration or publication,
merge the current local `main` into the feature branch and validate that exact
commit. Fast-forward a clean local `main`, then publish it explicitly with
`git push origin main:refs/heads/main`.

A single-branch clone can retain a narrow `origin` fetch refspec even after
`git push -u` succeeds. The preflight detects that state. Repair it once with:

```sh
git config remote.origin.fetch '+refs/heads/*:refs/remotes/origin/*'
git fetch --prune origin
```

Afterward, normal `origin/<branch>` comparisons work for feature branches. On a
clone that intentionally remains narrow, verify a publication directly with
`git ls-remote --heads origin refs/heads/<branch>`.

## Performance work

- Establish a representative baseline before optimizing, then compare complete
  workloads as well as isolated hot paths.
- Start with safe, idiomatic Rust and standard-library APIs; use unsafe code or
  architecture-specific intrinsics only for a measured bottleneck with a
  documented safety contract and validation.
- Treat latency, throughput, CPU time, peak memory, allocation count, binary
  size, startup time, and build time as separate metrics with explicit targets.
- Benchmark optimized builds and realistic inputs; debug-profile timing is not
  evidence of production performance.
