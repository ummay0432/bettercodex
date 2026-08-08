# bettercodex development workflows

Read this before changing Rust, tests, performance, or installation behavior.
bettercodex follows Codex's Cargo and `just` workflow, reduced to the one
retained Rust package. It does not carry Codex's Bazel, Node, release-packaging,
or Windows machinery.

## Setup and daily commands

Install the tools used by the upstream workflow:

```sh
rustup component add rustfmt clippy
cargo install --locked just
cargo install --locked cargo-nextest
just install
```

Build or run the binary through the checked-in Cargo wrapper:

```sh
./scripts/cargo-with-v8.sh build
./scripts/cargo-with-v8.sh run --bin bcodex -- "explain this codebase"
```

The wrapper downloads and verifies the sandbox-enabled V8 archive and generated
binding published by upstream Codex, then delegates every argument to Cargo.
Use it for commands that build bettercodex; formatting and dependency-only
commands can invoke Cargo directly.

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

## Finish a change

The primary checkout is shared integration state and must stay on `main`.

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
