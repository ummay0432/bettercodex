# Installing and building bettercodex

Bettercodex is one Rust package and one `bcodex` binary. It is built from the
private source repository with the Cargo workflow retained from upstream Codex;
there are no Bettercodex release binaries, hosted release builds, Bazel
workspace, or Node workspace.

## Supported systems

| Operating system | Architectures |
| --- | --- |
| macOS | Apple Silicon and Intel |
| Linux | ARM64 and x86-64 |

Windows is not supported.

## Install with a minimal retained footprint

Install an authenticated [GitHub CLI](https://cli.github.com/), `curl`, a native
C toolchain, and Rust through [rustup](https://rustup.rs/). Then copy and run the
repository's one-line [`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt).

The command shallow-clones the current private source into a temporary
directory. Cargo's dependency cache, compilation target, and verified V8
downloads are isolated under the same directory. An exit trap removes the
entire directory after a successful install, a build failure, or an
interruption. Only the binary under `$HOME/.local/bin`, its small Cargo
installation metadata, and BetterCodex user data remain from the BetterCodex
build. Rustup's selected Rust toolchain and the native toolchain are not removed
because other software may share them.

The source build still needs several gigabytes of free temporary space while it
runs. Eliminating the retained cache means every later install downloads and
compiles from scratch.

Open a new terminal if `$HOME/.local/bin` was not already on `PATH`, then
sign in and launch BetterCodex from a project directory:

```sh
bcodex login
bcodex
```

Use an existing Codex ChatGPT credential at
`${CODEX_HOME:-$HOME/.codex}/auth.json`, or sign in through `bcodex login`.
BetterCodex settings and saved sessions stay under `$HOME/.bcodex`.

## Updating

Bettercodex does not carry a separate updater or release-packaging system.
Fetch current source and reinstall by rerunning
[`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt). Each run uses a fresh shallow
clone, so an update cannot merge local source changes or reuse stale build
output.

## Migrating from the retired updater

An installed 0.1.2-era binary may still show `Update available` and advertise
`bcodex update`. That executable compares its embedded source commit with
private `main`, so any later commit triggers the notice even though both builds
report version 0.1.2. The updater itself was subsequently removed with the
private packaging scripts; an old executable cannot migrate across that removal
by updating itself.

Run [`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt) once to install current
source through Cargo. Existing ChatGPT credentials, Bettercodex settings, and
saved sessions are unaffected. After the newly installed binary launches, the
retired updater's build target and temporary source cache can be removed:

```sh
retired_cache="${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex"
cargo clean --target-dir "$retired_cache/build/target" &&
rm -rf "$retired_cache/build" "$retired_cache/tmp"
```

Those directories contain only the retired updater's downloaded source and
build artifacts; they do not contain credentials, settings, or sessions. The
minimal installer uses its own temporary V8 directory. A sibling `rusty-v8-*`
directory is needed only when a retained development checkout reuses it.

For a development checkout elsewhere on disk, use that clean `main` checkout
instead of cloning another copy:

```sh
git pull --ff-only &&
./scripts/cargo-with-v8.sh install --locked --path . --force \
  --root "$HOME/.local"
```

Run those commands from the Bettercodex repository root.

## Build without installing

From any Bettercodex source checkout, build or run the binary through the
checked-in Cargo wrapper:

```sh
./scripts/cargo-with-v8.sh build --locked
./scripts/cargo-with-v8.sh run --bin bcodex
```

Bettercodex enables V8's in-process sandbox. The matching archive is not in the
default `rusty_v8` release, so the wrapper follows current upstream Codex: it
downloads the matching archive and generated binding from the
`rusty-v8-v150.4.0` OpenAI Codex release, verifies their pinned SHA-256 digests,
caches them under `${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex`, and then runs
Cargo with both required overrides. The minimal installer points that cache at
its temporary directory; retained development checkouts use the persistent
default. Explicit paired overrides and `V8_FROM_SOURCE=1` remain authoritative.

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
the complete contribution procedure.
