# Contributing

Bug reports, reproducible installation failures, and focused design feedback are
welcome in GitHub Issues. Search existing issues first and include the operating
system, architecture, `bcodex --version` output, expected behavior, and the
smallest safe reproduction you can provide. Never include credentials or
private session data.

bettercodex is a focused downstream port maintained separately from OpenAI
Codex. External code contributions are currently accepted by prior agreement
only. Open an issue before investing in a pull request so scope and the upstream
baseline can be agreed first.

## Development workflow

Install the pinned Rust toolchain's formatting and lint components plus the
upstream-style command helpers:

```sh
rustup component add rustfmt clippy
cargo install --locked just
cargo install --locked cargo-nextest
just install
```

Retained Codex behavior should be ported from current upstream source rather
than reimplemented. Keep bettercodex's fixed product boundaries in
[`progressive_disclosure/product-direction.md`](../progressive_disclosure/product-direction.md)
and follow the complete workflow in
[`progressive_disclosure/development.md`](../progressive_disclosure/development.md).

Before proposing an agreed change, run:

```sh
just fix
just fmt
just test
just clippy -- -D warnings
./scripts/cargo-with-v8.sh build --release --locked
```

Document user-visible behavior and keep commits focused. By submitting a
contribution, you agree that it is licensed under this repository's
[Apache-2.0 license](../LICENSE).

Report security vulnerabilities through the private process in
[`SECURITY.md`](../SECURITY.md), not through a public issue.
