# Installing and building bettercodex

bettercodex is one Rust package and one `bcodex` binary. Public installations
track the exact current commit on public `main`. Cargo package versions are
display metadata; they do not decide whether an installation is current.

## Supported systems

| Operating system | Architectures |
| --- | --- |
| macOS 12+ | Apple Silicon and Intel |
| Linux with glibc 2.31+ | ARM64 and x86-64 |

Windows is not supported. A public install requires
[rustup](https://rustup.rs/), a working native C compiler, several gigabytes of
temporary free space, and network access to GitHub. On a new Mac, install Xcode
Command Line Tools with `xcode-select --install` before retrying.

## Install

Run the repository's canonical [`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt):

```sh
curl -fsSL https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.sh | sh
```

The bootstrap downloads `scripts/install.sh` from public `main`. The installer
then:

1. Resolves `refs/heads/main` once to a full 40-character commit ID.
2. Downloads the immutable source archive for exactly that commit.
3. Uses the checked-in lockfile and pinned Rust toolchain.
4. Builds `bcodex` with the exact commit embedded in the binary.
5. Checks its version and embedded revision, initializes V8, and materializes
   every embedded system resource in isolated directories.
6. Copies the verified binary to a stage beside the destination and atomically
   renames it over the installed command.

The selected commit does not change if `main` advances while compilation is in
progress. The next launch or explicit update discovers the newer commit.

By default, the command is installed at `$HOME/.local/bin/bcodex`. The installer
adds a managed PATH block to the appropriate shell profile when necessary and
reports any older `bcodex` that appears earlier on `PATH`. Set the absolute
`BCODEX_INSTALL_DIR` environment variable to choose another binary directory.

After installation, open a new terminal when requested, then run:

```sh
bcodex login
bcodex
```

ChatGPT credentials remain under `${CODEX_HOME:-$HOME/.codex}`. bettercodex
settings and sessions remain under `${BCODEX_HOME:-$HOME/.bcodex}`; installing
or updating does not remove either directory.

## Update behavior

Distribution builds embed their full source revision. After the TUI renders its
first frame, bettercodex compares that revision with public `main` in a bounded,
failure-silent background request. A different commit displays both short
revision IDs and asks the operator to run:

```sh
bcodex update
```

The explicit command resolves public `main`. If the exact revision is already
installed, it exits without compiling. Otherwise it fetches the installer from
that same immutable commit and passes the pinned commit to it, so the script and
the source snapshot cannot drift apart. The installer reuses cached downloads
and compatible compiled dependencies, performs all verification again, and
atomically replaces the command. Restart a running TUI to use the new binary.

Update checks do not compare Cargo versions, Git tags, or GitHub Releases. Set
`BCODEX_SKIP_UPDATE_CHECK=1` to disable the background check. The explicit
`bcodex update` command remains available.

### Migrating binaries without the current updater

Binaries installed before this policy change cannot discover the current
updater on their own. Some older builds also parse the word `update` as an
interactive prompt, so do not probe support by running `bcodex update` and
waiting to see what happens. Check the command table first, then use the
canonical installer when support is absent:

```sh
if bcodex --help 2>&1 | grep -q '^  update '; then
    bcodex update
else
    curl -fsSL https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.sh | sh
fi
```

The one-time direct install preserves credentials, settings, sessions, and
reusable dependency caches. Afterward, normal notices and `bcodex update`
follow public `main` directly.

## Caching and cleanup

When a cache home is available, the installer retains reusable downloads and
one compatible compiled-dependency generation under
`${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex`. A changed toolchain, manifest,
lockfile, or V8 wrapper resets the incompatible compiled target instead of
accumulating generations. Package-owned binaries, fingerprints, incremental
output, source archives, compiler scratch space, and stage files are removed on
success and failure.

The installer refuses symlinked destinations and never follows cache symlinks
while cleaning. A lock rejects concurrent installs and records temporary state
so a later invocation can recover an orphan left by an untrappable crash. Any
failed download, extraction, build, smoke test, copy, or verification leaves an
existing installed binary untouched.

Without `HOME` or `XDG_CACHE_HOME`, dependency downloads and build output stay
inside the disposable installation tree and are removed afterward.

## Build from a checkout

Use the checked-in wrapper so the V8 archive and binding match the crate:

```sh
./scripts/cargo-with-v8.sh build --locked
./scripts/cargo-with-v8.sh run --bin bcodex
```

Development builds intentionally have no embedded distribution revision and do
not perform update checks. To install a checkout manually:

```sh
./scripts/cargo-with-v8.sh install --locked --path . --force \
  --root "$HOME/.local"
```

Use [`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt), not a development build,
for an installation that follows public `main`.

## Development checks

Install `just` and Cargo Nextest, then run:

```sh
just fix
just fmt
just test
just clippy -- -D warnings
python3 scripts/install_tests.py
```

See [the development workflow](../progressive_disclosure/development.md) for
the complete contribution procedure.
