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
C toolchain, and Rust through [rustup](https://rustup.rs/). On a new Mac,
install Xcode or Apple's
[Command Line Tools](https://developer.apple.com/documentation/xcode/installing-the-command-line-tools/)
first:

```sh
xcode-select --install
gh auth login --hostname github.com
```

The GitHub account must have access to the private repository. Then copy and
run the repository's one-line
[`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt). Its preflight reports missing
GitHub CLI or authentication before creating a temporary file.

The one-line bootstrap always fetches the canonical `scripts/install.sh` from
private `main`. That installer resolves `main` to a 40-character commit ID,
downloads GitHub's source archive for that exact commit, and uses the checked-in
lockfile and pinned Rust toolchain for a release build. This avoids a Homebrew
or system Cargo earlier on `PATH` silently selecting a different compiler. An
already-installed matching Rust toolchain is reused. If that exact toolchain is
missing, rustup downloads its minimal profile inside the temporary install tree
instead of adding another persistent toolchain.

Source, Cargo's dependency cache, the compilation target, compiler scratch
space, and verified V8 downloads all live under one temporary install tree.

Before replacement, the candidate binary must report the expected package
version and embedded source commit, initialize V8, and materialize every
embedded system resource in an isolated home. The installer then checks
private `main` again. If `main` advanced during the build, it discards that
source and target and retries the newer commit, up to three attempts. Only a
candidate matching the final check is renamed over `bcodex`; the rename is in
the destination directory, and the staged and installed bytes are compared with
the verified build. A failed build, copy, or smoke test leaves the existing
binary untouched. Transient GitHub revision and source-download failures are
also retried three times; partial archives are discarded between attempts.

An exit trap removes the complete temporary install tree after success, build
failure, `SIGHUP`, `SIGINT`, or `SIGTERM`. A lock records the tree so the next
install can remove it after an untrappable crash or `SIGKILL`. Only the binary
under `$HOME/.local/bin`, a small shell-profile PATH entry when needed, and
BetterCodex user data remain from the BetterCodex build. The prerequisite
rustup installation, any matching toolchain that existed before the install,
and the native compiler are left alone because other software may share them.
An installer-only pinned toolchain is removed with the temporary tree.

The source build still needs several gigabytes of free temporary space while it
runs. Eliminating the retained cache means a later update normally downloads
dependencies and compiles from scratch. Retries within one installer process
may reuse that process's temporary dependency and V8 downloads, but never an
older compilation target; the entire tree is removed at the end.

Open a new terminal if `$HOME/.local/bin` was not already on `PATH`, then
sign in and launch BetterCodex from a project directory:

```sh
bcodex login
bcodex
```

If another `bcodex` earlier on `PATH` would still launch an older binary, the
installer reports its path, prepends the managed install directory for future
terminals, and prints the `export PATH=...` command for the current terminal.
Managed PATH-block updates preserve the profile's permissions. If replacing a
symlinked profile or safely rewriting its block is not possible, installation
still succeeds and reports the manual PATH step instead.

Use an existing Codex ChatGPT credential at
`${CODEX_HOME:-$HOME/.codex}/auth.json`, or sign in through `bcodex login`.
BetterCodex settings and saved sessions stay under `$HOME/.bcodex`.

## Updating

The installer embeds its exact source revision in the binary. After the TUI
renders its first frame, an installed release compares that revision with the
current private `main` commit in a failure-silent background check. This uses
the source commit, not the package version, so improvements are detected even
while both binaries report the same `bcodex` version. When they differ, the TUI
shows both short commit IDs and the command to run in another terminal:

```sh
bcodex update
```

`bcodex update` first resolves private `main`. If its exact commit is already
installed, it exits without rebuilding. Otherwise it fetches the installer
from that observed commit, runs the same verified temporary build, and targets
the directory containing the running executable. An install lock rejects a
second concurrent update. The running TUI keeps using its old in-memory code
until restarted; new processes use the atomically replaced binary. Failed
background checks stay silent and are retried on the next launch; set
`BCODEX_SKIP_UPDATE_CHECK=1` to disable them. Rerunning
[`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt) remains an equivalent manual
update path for the default `$HOME/.local/bin` installation.

Each update uses a fresh immutable source archive, so it cannot merge local
source changes or reuse stale build output.

### What other devices can see

Private `main` on GitHub is the distribution channel. Work that exists only in
this development checkout—uncommitted changes, an unpushed commit, or a commit
on another branch—is intentionally invisible to installed devices. Commit the
finished change and push it to `ummay0432/bettercodex` `main`; the next TUI
launch on another signed-in device will compare against that commit, and
`bcodex update` will install it. A commit pushed after an installer's final
check is a new update, not silent drift, and is detected on the next launch.

### Cleanup and embedded-file integrity

Current installs and updates automatically remove the retired updater's
`${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex/build` and `tmp` directories. They
do this before compiling so the old multi-gigabyte target cannot crowd out the
temporary replacement build. They do not remove credentials, sessions,
settings, a development checkout's `rusty-v8-*` cache, Rust toolchains, or
native compiler tools that existed before the install. A missing pinned Rust
toolchain created solely for an install is temporary. Cache paths that are
symbolic links are left untouched rather than followed.

Embedded system skills are checked on launch independently of the source
revision marker. BetterCodex verifies every expected file's bytes and private
permissions and rejects unexpected entries under the reserved
`$BCODEX_HOME/skills/.system` tree. Missing, edited, permission-drifted, and
retired files are replaced through a staged directory swap; a previous complete
tree is restored if that swap was interrupted. Put operator-created skills
beside `.system`, not inside that reserved directory.

## Migrating from an older updater

An installed 0.1.2-era binary may still show `Update available` and advertise
`bcodex update`. That executable compares its embedded source commit with
private `main`, so any later commit triggers the notice even though both builds
report version 0.1.2. Some older executables embed the retired persistent-cache
installer and cannot migrate across its removal by updating themselves.

Run [`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt) once to install current
source. Existing ChatGPT credentials, Bettercodex settings, and saved sessions
are unaffected. The newly installed binary restores `bcodex update` with the
self-cleaning installer and removes the retired updater's build and temporary
source directories automatically.

For a development checkout elsewhere on disk, use that clean `main` checkout
instead of cloning another copy:

```sh
git pull --ff-only &&
./scripts/cargo-with-v8.sh install --locked --path . --force \
  --root "$HOME/.local"
```

Run those commands from the Bettercodex repository root.
Development builds intentionally have no embedded distribution revision and do
not perform update checks. Use [`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt)
for an operator/device installation that must track pushed private `main`.

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
python3 scripts/install_tests.py
```

See [the development workflow](../progressive_disclosure/development.md) for
the complete contribution procedure.
