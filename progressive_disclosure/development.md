# BetterCodex development workflows

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

Every Rust change must pass all three commands; treat every Clippy warning as an
error:

```sh
./scripts/dev.py cargo fmt --all
./scripts/dev.py cargo test
./scripts/dev.py cargo clippy --all-targets -- -D warnings
```

Cargo can wait on a shared build lock. Let it finish; do not kill a Cargo or
Rust process by PID to make the lock disappear.

After those commands pass, install the current worktree:

```sh
cargo install --locked --path . --force --root "$HOME/.local"
```

Then run the relevant smoke test against `$HOME/.local/bin/bcodex`. Testing only
`target/debug/bcodex` or `target/release/bcodex` does not finish a BetterCodex
code change.

## Cargo worktrees and disk space

`./scripts/dev.py cargo …` gives every linked Git worktree a distinct Cargo
target under the primary worktree's `target/worktrees/` directory. This avoids
executing a package artifact produced from another concurrently changing
worktree, while keeping large build output on the primary worktree's filesystem
instead of a space-constrained `/tmp` mount. The primary worktree continues to
use its normal `target/` directory.

Do not point `CARGO_TARGET_DIR` at another active worktree. Use the helper for
focused tests and for the complete validation sequence above.

`./scripts/dev.py preflight` reports free space, the selected target, inactive
per-worktree target directories, BetterCodex test/temp directories older than
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

## Remote branch verification

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
