# bettercodex development

Read this before changing Rust, tests, build scripts, performance, or
installation behavior.

## Commands

Install the pinned toolchain components and upstream-style helpers:

```sh
rustup component add rustfmt clippy
cargo install --locked just
cargo install --locked cargo-nextest
just install
```

Build through the checked-in V8 wrapper. Raw Cargo can request an unpublished
debug V8 archive.

```sh
./scripts/cargo-with-v8.sh build
./scripts/cargo-with-v8.sh run --bin bcodex -- "explain this codebase"
```

On native Windows, use `scripts/cargo-with-v8.ps1`. Routine validation mirrors
upstream Codex:

```sh
just fmt
just fix
just test
just clippy -- -D warnings
```

`just test <filter>` forwards arguments to Cargo Nextest. If Nextest is not
available, use:

```sh
RUST_MIN_STACK=8388608 ./scripts/cargo-with-v8.sh test --bin bcodex <filter>
```

This package has no library target, so `cargo test --lib` is invalid.

## Source rules

- Port retained behavior from current upstream Codex before editing it here.
- Keep the one-package, one-binary design; do not add build or release
  infrastructure for workflows bettercodex does not retain.
- Keep target dependencies and platform code target-gated.
- Prefer direct code over a generic abstraction with one caller.
- Keep modules private unless another module needs an export.
- Prefer named methods, enums, or small types over ambiguous boolean and
  `Option` parameters.
- Let rustfmt and Clippy enforce mechanical style.

## Tests

- Retain upstream tests with retained behavior.
- Before adding local coverage, inspect current upstream tests and the nearest
  existing bettercodex test and helpers. Port or extend that coverage rather
  than creating a parallel harness, fixture family, or validation script.
- Add a bettercodex-only test only for an intentional product departure or a
  concrete regression that upstream cannot cover.
- Test observable behavior rather than implementation details, copied text, or
  static values.
- Do not add a negative test whose only assertion is that removed logic remains
  absent.
- Reuse existing test helpers and avoid adding production APIs or functions
  solely for tests.
- Prefer complete-value equality over many field assertions.
- Keep new test modules in sibling `*_tests.rs` files.
- Do not disguise manual benchmarks as tests.

## Shared checkout and artifacts

The checkout and its warm `target/` may be shared. Cargo coordinates concurrent
writers; never delete the shared target or another session's artifacts. An
isolated validation or benchmark must use a unique task-owned target on a
disk-backed filesystem and remove only that target after its Cargo processes
finish, including on failure.

Do not kill Cargo or rustc to take a build lock. Before a clean isolated build,
check available disk space; a full target can require several gigabytes.

## Final validation

Start with the narrowest existing test filter and checks that exercise the
changed path. Expand validation according to dependency reach, using existing
commands rather than creating a persistent validation mechanism for a one-off
task.

For cross-cutting changes, release or installation behavior, or another change
whose dependency reach warrants it, run the complete local gate when feasible:

```sh
just fix
just fmt
just test
just clippy -- -D warnings
./scripts/cargo-with-v8.sh build --release --locked
```

Run the prebuilt installer tests on their native platforms. Windows source and
terminal validation still requires a native Windows machine with MSVC; a Rust
target installed on Linux is not a useful substitute.

## Native Windows qualification

Before native Windows leaves developer preview, validate on x86-64 Windows 11
with MSVC:

- pipe and ConPTY execution, including stream separation or merging, resize,
  interruption, timeout, descendant cleanup, and non-ASCII paths;
- Windows Terminal and VS Code's integrated terminal, including keyboard input,
  ordinary and bracketed paste, IME input, clipboard operations, hyperlinks,
  resize and reflow, resume, login, and clean shutdown;
- public installation and updates from clean and existing profiles, including
  rollback and locked-file failures; and
- platform-specific model context isolation and cleanup of every task-owned
  process, stage, cache, lock, and temporary file after failures.

The manual release workflow is the only distribution build path. It must build
all three targets from one public `main` revision, qualify the shared Linux
binary on Ubuntu and Debian, and create a draft with exactly the five compressed
assets listed in [`releasing.md`](releasing.md). The two zstd assets are
compatibility encodings of the macOS and Linux binaries for immutable clients
older than 0.1.3.
