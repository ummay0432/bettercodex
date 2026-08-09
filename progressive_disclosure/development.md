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
- Add a bettercodex-only test only for an intentional product departure or a
  concrete regression that upstream cannot cover.
- Test observable behavior rather than implementation details, copied text, or
  static values.
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

Run the checks relevant to the changed surface, then the complete local gate
when feasible:

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

The manual release workflow is the only distribution build path. It must build
all three targets from one public `main` revision, qualify the shared Linux
binary on Ubuntu and Debian, and create a draft with exactly the three
compressed assets named in `SPEC.md`.
