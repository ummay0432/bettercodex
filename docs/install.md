# Installing and building bettercodex

bettercodex is one Rust package and one `bcodex` binary. Users install verified
native binaries from GitHub Releases; maintainers retain the upstream-derived
Cargo workflow for development and exceptional source fallback. There is no
Node workspace, npm package, Bazel build, GitHub Actions release build, or
GitHub Packages dependency.

The repository is public before its first complete four-platform native release
has been published. Until that release is available, the same installer uses
the documented source fallback and requires rustup, a native C toolchain,
several gigabytes of free space, and more time. It automatically selects native
assets once a compatible complete release exists.

## Supported systems

| Operating system | Architectures |
| --- | --- |
| macOS 12+ | Apple Silicon and Intel |
| Linux with glibc 2.31+ | ARM64 and x86-64 |

Windows is not supported.

## Run the installer

Copy and run the repository's one-line
[`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt). It fetches the installer over
HTTPS without requiring a GitHub account or GitHub CLI login.

The one-line bootstrap always fetches the canonical `scripts/install.sh` from
public `main`. The installer resolves the latest complete GitHub Release, maps
the current operating system and architecture to one of four native targets,
and uses that response's GitHub-calculated SHA-256 to download one asset. It
prefers `bcodex-<target>.xz` when the standard XZ tool is available, then zstd
when available, and keeps `bcodex-<target>.gz` as the portable fallback. On the
reviewed Linux x86-64 binary those choices are about 16.1 MB, 17.2 MB, and
21.6 MB respectively. The release metadata response itself is HTTP-compressed.

Before replacement, the installer verifies the compressed bytes, decompresses
into a stage file beside the destination, and requires the candidate to report
the release's exact package version and full source revision. It then
initializes V8 and materializes every embedded system resource in an isolated
home. The selected tag and assets are immutable, so no second release lookup is
needed; a newer release published during installation is found by the next
background check. Only a fully verified stage is atomically renamed over
`bcodex`.

A failed metadata request, download, digest, decompression, runtime check, copy,
or smoke test leaves the existing binary untouched. Requests retry three times;
the installer never redownloads a complete valid asset merely because a newer
release appeared. A lock rejects concurrent installs and records temporary
state so the next invocation can recover files left by an untrappable crash or
`SIGKILL`.

The normal retained footprint is the installed executable, a small
shell-profile PATH entry when needed, and bettercodex user data. Downloaded
release files and staging trees are removed on success or failure. Rust, Cargo,
a native compiler, npm, Homebrew, and GitHub CLI are not required for users.

Open a new terminal if `$HOME/.local/bin` was not already on `PATH`, then
sign in and launch bettercodex from a project directory:

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
bettercodex settings and saved sessions stay under `$HOME/.bcodex`.

## Updating

The installer embeds its exact source revision in the binary. After the TUI
renders its first frame, an installed release compares that revision with the
latest complete, non-prerelease GitHub Release in a failure-silent background
check. Development commits on public `main` do not trigger notices before
verified native assets exist. When release revisions differ, the TUI shows both
short commit IDs and the command to run in another terminal:

```sh
bcodex update
```

`bcodex update` resolves that release. If its exact source revision is already
installed, it exits after one bounded metadata request. Otherwise the running
binary selects this computer's direct `.zst` asset from the same response. When
the release includes a valid patch from the installed revision that is at least
10% smaller than the full transfer, it tries that transfer first; any patch
failure falls back to the full asset.

The updater streams the compressed bytes through its built-in zstd decoder into
a stage beside the installed executable. It verifies GitHub's exact asset size
and SHA-256 while streaming, caps decompressed output, and then checks the
candidate's version, source revision, V8 runtime, and embedded resources before
an atomic replacement. It does not download a shell script or checksum file,
and it never invokes Cargo or source fallback. The reviewed Linux x86-64 build
is about 17.2 MB as a full update; the measured predecessor patch for the
reviewed build was about 2.4 MB.

The running TUI keeps using its old in-memory code until restarted; new
processes use the atomically replaced binary. Failed background checks stay
silent and are retried on the next launch; set `BCODEX_SKIP_UPDATE_CHECK=1` to
disable them. Rerunning [`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt) remains
the bootstrap and migration path for the default `$HOME/.local/bin`
installation, but release-aware binaries use the faster direct updater.

The release tag encodes both package version and the immutable full source
revision. Published tags and assets are never replaced; a bad release is fixed
with a new source revision.

### Source fallback

The bootstrap installer enters a local source build only when no release exists
yet, the native bootstrap asset is confirmed missing, or a verified asset
cannot run on the local OS baseline. Ordinary network failures are retried and
reported; they never silently become a multi-gigabyte build. `bcodex update`
never enters this path: a missing or invalid direct-update asset leaves the
working executable intact and reports the release error.

The fallback clearly reports that it needs Rust through
[rustup](https://rustup.rs/), a native C toolchain, and several gigabytes of
free space. It downloads the immutable released source, uses the checked-in
lockfile and pinned Rust toolchain, and builds that once-resolved commit only
once even if public `main` advances during compilation. It retains one
compatible compiled-dependency generation under
`${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex/build/<native-target>/target`.
Source and compiler scratch space remain temporary, and bettercodex-owned
outputs are removed after verification. A changed toolchain, manifest,
lockfile, or V8 wrapper replaces the old generation instead of accumulating
another one.

### What other devices can see

Public Git branches and tags are visible to everyone, but only a complete
published GitHub Release is installed. Work that is uncommitted, unpushed, on a
branch, on `main`, or still in a draft release cannot trigger another device.
This prevents users from being sent to a source build while platform assets are
still being prepared.

### Cleanup and embedded-file integrity

A successful native release install removes the retired source updater's known
`build`, `cargo`, and `tmp` cache directories, reclaiming the compiler output
and unpacked package sources that previously consumed gigabytes. It never
follows cache symlinks or removes credentials, sessions, settings, arbitrary
siblings, Rust toolchains, or native compilers. The small `rusty-v8-*` cache is
retained because development checkouts intentionally share it.

Embedded system skills are checked on launch independently of the source
revision marker. bettercodex verifies every expected file's bytes and private
permissions and rejects unexpected entries under the reserved
`$BCODEX_HOME/skills/.system` tree. Missing, edited, permission-drifted, and
retired files are replaced through a staged directory swap; a previous complete
tree is restored if that swap was interrupted. Put operator-created skills
beside `.system`, not inside that reserved directory.

## Migrating from an older updater

An older binary may compare its embedded source commit directly with `main` and
may still launch the old source compiler. Run
[`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt) once after the first complete
native release is published. Existing ChatGPT credentials, bettercodex
settings, and saved sessions are unaffected. The release-aware binary then
tracks only published releases and removes the retired source-build cache after
a successful native install.

For a development checkout elsewhere on disk, use that clean `main` checkout
instead of cloning another copy:

```sh
git pull --ff-only &&
./scripts/cargo-with-v8.sh install --locked --path . --force \
  --root "$HOME/.local"
```

Run those commands from the bettercodex repository root.
Development builds intentionally have no embedded distribution revision and do
not perform update checks. Use [`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt)
for an installation that tracks published releases.

## Publishing native assets

Release builds are explicit maintainer operations; this repository does not use
GitHub Actions or GitHub Packages for them. Create an annotated
`bcodex-v<version>-<full-revision>` tag and a draft GitHub Release, then run on
each trusted native build host:

```sh
scripts/publish-release.sh /absolute/inspection/directory
scripts/publish-release.sh --upload
```

The command uses a disposable Cargo target, the pinned toolchain, the
size-focused `distribution` profile, and the full embedded revision. It runs
the install smoke test, remaps and rejects private build-host paths, enforces the
Linux glibc compatibility floor, verifies macOS signatures when present, and
creates portable gzip, smaller XZ bootstrap, and fast zstd update assets plus
their SHA-256 files. If a previous release exists, it also verifies that
target's old binary and creates a byte-exact raw-prefix patch when that patch
saves at least 10% over the full zstd update. It uploads only to an existing
draft. Publishing that draft remains a separate manual action after all four
targets have been inspected. See
[`spec-install.md`](../spec-install.md) for the complete release contract.

## Build without installing

From any bettercodex source checkout, build or run the binary through the
checked-in Cargo wrapper:

```sh
./scripts/cargo-with-v8.sh build --locked
./scripts/cargo-with-v8.sh run --bin bcodex
```

bettercodex enables V8's in-process sandbox. The matching archive is not in the
default `rusty_v8` release, so the wrapper follows current upstream Codex: it
downloads the matching archive and generated binding from the
`rusty-v8-v150.4.0` OpenAI Codex release, verifies their pinned SHA-256 digests,
caches them under `${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex`, and then runs
Cargo with both required overrides. Development checkouts and source fallbacks
share that persistent V8 cache. A fallback also keeps Cargo downloads in the
sibling `cargo` directory and compatible compiled dependencies below `build`;
a later native release install can reclaim both source-build caches. Explicit
paired overrides and `V8_FROM_SOURCE=1` remain authoritative.

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
