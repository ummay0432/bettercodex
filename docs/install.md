# Installing and building bettercodex

Bettercodex is one Rust package and one `bcodex` binary. It is built from the
private source repository with the same Cargo workflow documented by upstream
Codex; there are no prebuilt releases, hosted release builds, Bazel workspace,
or Node workspace.

## Supported systems

| Operating system | Architectures |
| --- | --- |
| macOS | Apple Silicon and Intel |
| Linux | ARM64 and x86-64 |

Windows is not supported.

## Build from source

Install Git, a native C toolchain, and Rust through
[rustup](https://rustup.rs/). Then clone the private repository and build:

```sh
gh repo clone ummay0432/bettercodex
cd bettercodex
cargo build --locked
cargo run --bin bcodex
```

To install the binary under `$HOME/.local/bin`:

```sh
cargo install --locked --path . --force --root "$HOME/.local"
```

Open a new terminal if `$HOME/.local/bin` was not already on `PATH`, then
sign in and launch BetterCodex from a project directory:

```sh
bcodex login
bcodex
```

Use an existing Codex ChatGPT credential at
`${CODEX_HOME:-$HOME/.codex}/auth.json`, or sign in through `bcodex login`.
BetterCodex settings and saved sessions stay under `$HOME/.bcodex`.

## Development checks

Install `just` and Cargo Nextest, then use the Cargo-facing recipes retained
from upstream:

```sh
cargo install --locked just
cargo install --locked cargo-nextest
just fmt
just fix
just test
just clippy -- -D warnings
```

See [the development workflow](../progressive_disclosure/development.md) for
the complete contribution and worktree procedure.
