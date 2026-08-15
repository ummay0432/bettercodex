# Installing and building bettercodex

Public bettercodex installations use prebuilt binaries from GitHub Releases.
They do not download source code or require Rust, Cargo, or native build tools.

## Supported systems

| System | Architecture | Status |
| --- | --- | --- |
| macOS 12 or newer | Apple silicon | Supported |
| Ubuntu 22.04+ and Debian 12+ | x86-64 | Supported |

Ubuntu and Debian share the `x86_64-unknown-linux-gnu` release binary.

## Install

On macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.sh | sh
```

Open a new terminal when requested, then run:

```sh
bcodex
```

On the first interactive launch, bettercodex asks you to sign in only when no
valid Codex credentials are available. Existing credentials remain valid across
installs and updates.

The installer requires `curl`, `gzip`, and standard POSIX utilities. It selects
the matching asset from the latest published full release, rejects unexpected
sizes or binary identities, and replaces the installed command only after
verification succeeds. On macOS it also verifies the binary's code signature.
Release Actions smoke-test each binary on its native runner before it can become
a release asset.

The default command location is `$HOME/.local/bin/bcodex`.

Set an absolute `BCODEX_INSTALL_DIR` to choose another directory. The installer
adds the default directory to the user's `PATH` when needed. It does not remove
credentials, settings, or sessions.

A successful install leaves one executable plus a `PATH` entry only when one
was needed. Download archives, staged binaries, and locks are transaction-local
and removed. When migrating from the retired source-building installer, the
installer also removes its recognized compiler, toolchain, dependency, and
private ripgrep caches. A standalone V8 cache is preserved when there is no
evidence that the installer owns it, because it may belong to a developer
checkout.

## Releases and updates

Every published binary embeds a tag of the form
`bcodex-v<version>-<40-character-source-revision>`. The semantic version decides
whether a newer full release is available; the revision pins the exact source
and installer used for that release.

After the TUI renders, published and optimized source builds perform one
bounded, failure-silent check against GitHub's latest non-draft, non-prerelease
release. Set `BCODEX_SKIP_UPDATE_CHECK=1` to disable that background check.
Debug builds stay offline.

To replace an older published or optimized source build with the latest
published release, run:

```sh
bcodex update
```

The updater validates the latest release metadata and target asset, fetches the
installer from the immutable source revision encoded in that release tag, and
installs the matching prebuilt binary. It never compiles locally and replaces
the binary atomically.

`BCODEX_REPOSITORY` selects another `owner/repository` for development or fork
testing. `BCODEX_INSTALL_RELEASE_TAG` pins an exact asset and is reserved for the
updater and release validation.

## State directories

`CODEX_HOME` and `BCODEX_HOME` override credential and bettercodex state
directories on both supported systems. Without overrides, they are
`$HOME/.codex` and `$HOME/.bcodex`.

## Build from a checkout

Source compilation is a developer workflow:

```sh
cargo build --locked
cargo run --bin bcodex
```

Development binaries intentionally have no embedded release tag. Optimized
source builds still check the published release version and support
`bcodex update`; debug builds do neither.

## Development checks

The routine local gate mirrors upstream Codex:

```sh
just fix
just fmt
just test
just clippy -- -D warnings
cargo build --release --locked
```

The compact installer suites cover bettercodex's intentional prebuilt-release
departure:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/install_tests.py
```

See [the development workflow](development.md) for source and artifact rules.
