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

The installer requires `/bin/sh`, `curl`, `gzip`, `mktemp`, standard POSIX
utilities, and one SHA-256 implementation: `sha256sum`, `shasum`, or `openssl`.
It performs no compilation.

Before downloading a binary, the installer accepts only a bounded GitHub
response describing one full, immutable release. The release tag must contain a
plain `major.minor.patch` version and a full lowercase source revision;
`target_commitish` must equal that encoded revision. The native gzip asset must
appear exactly once, be in the `uploaded` state, have a nonzero size no greater
than 128 MiB, and expose a lowercase `sha256:` digest.

The metadata and archive downloads ignore per-user curl configuration and have
finite connection, retry, redirect, time, and hard file-size limits that do not
depend on server `Content-Length` headers. The archive's actual byte count and
SHA-256 must match the release metadata before decompression or binary
execution. Decompression itself is
capped at 128 MiB. On macOS, code-signature verification also happens before
execution. The staged binary must then report the exact selected tag, version,
and source revision.

The default command location is `$HOME/.local/bin/bcodex`. Set an absolute
`BCODEX_INSTALL_DIR` with no `..` components to choose another directory. The
selected directory must not be a Cargo target/profile tree, including through a
symlinked alias. When the default directory is missing from `PATH`, the installer
updates `.bashrc` or `.zshrc` on Linux, the corresponding login profile on
macOS, and `.profile` for other POSIX shells. Custom locations are left for the
user to add. Paths containing non-ASCII, nonprinting, or non-UTF-8 bytes are
reported as hexadecimal byte sequences rather than being emitted unsafely to
the terminal.

Installation uses a same-directory transaction and install lock. Only the final
rename replaces `bcodex`, so download, checksum, decompression, signature,
identity, and staging failures leave an existing command unchanged. Success is
reported only after that rename completes. Transaction files and locks are
transaction-local; cleanup runs on both success and failure and warns if the
filesystem prevents removal.

The installer does not remove credentials, settings, or sessions. When
migrating from the retired source-building installer, it removes only recognized
compiler, toolchain, dependency, and private-ripgrep caches. It refuses to
traverse symlinked cache roots. A standalone V8 cache is preserved when there is
no evidence that the installer owns it because it may belong to a developer
checkout.

## Releases and update checks

Every published binary embeds a tag of the form
`bcodex-v<version>-<40-character-source-revision>`. The version controls normal
update ordering; the revision pins the exact release source and installer.

After the first TUI frame renders, published binaries and release-profile source
builds perform at most one 10-second, failure-silent check of the selected
repository's latest full release. A notice appears only when that release has a
strictly newer version, all metadata needed for the native installer is valid,
and the eventual install destination passes the same policy as an explicit
update. Set `BCODEX_SKIP_UPDATE_CHECK=1` (or any value) to omit the task entirely.
Debug builds never create the task or contact GitHub.

The notice pins the selected repository and resolved install directory, then
runs the exact current executable rather than resolving another `bcodex` from
`PATH`. Printable ASCII paths are shell-quoted; Unicode, nonprinting, and
non-UTF-8 paths are represented by a POSIX-shell byte-safe command.

## Explicit update policy

Run:

```sh
bcodex update
```

The command uses the same build classification as CLI help and TUI notices:

| Build | Explicit behavior | Destination |
| --- | --- | --- |
| Debug source build | Rejected without network access | None |
| Release-profile source build | Installs the selected published release even when the source package version is equal or newer | `BCODEX_INSTALL_DIR` or `$HOME/.local/bin` |
| Published build | Updates only when the default channel is newer; exact, older, and same-version/different-revision cases are reported explicitly | The running `bcodex` directory |

A source-build update refuses the running checkout binary, its detected Cargo
artifact tree, and any destination recognizable as a Cargo target/profile tree.
It therefore installs through the release channel without replacing shared
Cargo outputs. A published update requires the running executable to be named
`bcodex`, refuses Cargo artifact locations, and replaces that executable in
place. An inherited `BCODEX_INSTALL_DIR` cannot redirect it; it must be unset or
resolve to the running executable's directory.

`BCODEX_REPOSITORY` selects another validated `owner/repository` release channel
for fork or development testing. For an explicit update, this override
intentionally selects that repository's release even when its version is equal
to or older than the current version; an exact embedded tag remains a no-op.
Without a repository override, a newer release installs, an exact tag is already
current, an older release leaves the current binary alone, and a reused version
with a different known source revision is rejected.

The updater resolves and validates the release once, fetches `scripts/install.sh`
from the exact encoded source revision, checks a fixed installer prefix and a
1 MiB response limit, then invokes `/bin/sh`. Before spawning that child, it
removes inherited installer-selection variables and sets exactly the selected
repository, release tag, asset size, asset SHA-256, and install directory. The
installer uses that attestation to avoid a second metadata request while still
verifying the downloaded archive bytes before execution.

`BCODEX_INSTALL_RELEASE_TAG` can pin an exact release when invoking the
standalone installer directly. `BCODEX_INSTALL_ASSET_SHA256` and
`BCODEX_INSTALL_ASSET_SIZE` are paired internal updater inputs; they are not
release-selection controls for normal users. `bcodex update` never inherits a
stale release tag or asset attestation from its parent environment.

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

Development binaries intentionally have no embedded release tag. A standard
release-profile build can check for a newer published version and use its exact
notice command to install through the release channel. A debug build does
neither.

## Development checks

The routine local gate mirrors upstream Codex:

```sh
just fix
just fmt
just test
just clippy -- -D warnings
cargo build --release --locked
```

The installer suite uses local fixtures and mocked GitHub endpoints:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/install_tests.py
```

See [the development workflow](development.md) for source and artifact rules.
