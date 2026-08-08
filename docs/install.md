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

## Install from source

Install an authenticated [GitHub CLI](https://cli.github.com/), a native C
toolchain, and Rust through [rustup](https://rustup.rs/). Then clone the private
repository into a stable local path and install the package:

```sh
bcodex_source="${XDG_DATA_HOME:-$HOME/.local/share}/bettercodex/source"
mkdir -p "$(dirname "$bcodex_source")" &&
gh repo clone ummay0432/bettercodex "$bcodex_source" &&
cargo install --locked --path "$bcodex_source" --force --root "$HOME/.local"
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

## Updating

Bettercodex does not carry a separate updater or release-packaging system. Pull
the retained source checkout forward and reinstall the same Cargo package:

```sh
bcodex_source="${XDG_DATA_HOME:-$HOME/.local/share}/bettercodex/source"
git -C "$bcodex_source" pull --ff-only &&
cargo install --locked --path "$bcodex_source" --force --root "$HOME/.local"
```

`git pull --ff-only` stops instead of merging when the checkout has diverged;
the chained Cargo command therefore installs only the requested revision.
The one-line [`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt) performs either the
first clone or this update against the same checkout.

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
retired updater's dedicated build cache is unused and can be removed:

```sh
rm -rf "${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex"
```

The cache contains only the retired updater's downloaded source and build
artifacts; it does not contain credentials, settings, or sessions.

For a development checkout elsewhere on disk, use that clean `main` checkout
instead of cloning another copy:

```sh
git pull --ff-only &&
cargo install --locked --path . --force --root "$HOME/.local"
```

Run those commands from the Bettercodex repository root.

## Build without installing

From any Bettercodex source checkout, build or run the binary directly with
Cargo:

```sh
cargo build --locked
cargo run --bin bcodex
```

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
