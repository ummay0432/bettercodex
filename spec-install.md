# BetterCodex installation and update specification

Status: proposed

This document defines the target installation and update system for
BetterCodex. The primary user path distributes verified native binaries. A
local Cargo build exists only as a compatibility fallback.

## Outcome

For a supported published target, both a first install and `bcodex update`
must be a binary download and atomic replacement. The user must not need
Rust, Cargo, a C compiler, a source checkout, or gigabytes of build space.

The normal update path must:

1. resolve one published BetterCodex release;
2. select the native asset;
3. download roughly one compressed binary;
4. verify its digest, version, embedded source revision, and runtime resources;
5. atomically replace the installed executable; and
6. remove the downloaded archive and staging files.

It must compile zero Cargo packages.

The current Linux x86-64 `bcodex` executable is 67,385,384 bytes and compresses
to 25,392,267 bytes with `gzip -9`. These are observations, not fixed format
limits, but they establish the expected order of magnitude. The roughly
2.5 GB seen during a source update is predominantly locally generated compiler
output, not network download volume.

## Why the installed Codex executable cannot be reused

The useful part of upstream Codex's design is its distribution model, not its
installed binary.

The current standalone Codex installation contains a stripped, statically
linked native executable. The `@openai/codex` npm package is a JavaScript
launcher which selects another platform-specific package containing that same
kind of native executable. Neither installation exposes Rust libraries, Cargo
artifacts, a stable extension ABI, or linkable object files.

BetterCodex contains its own inference loop, tool runtime, terminal UI, saved
session behavior, and embedded resources. Cargo cannot graft those Rust changes
onto Codex's already-linked executable. Turning BetterCodex into a wrapper
around `codex` would be a different product and would surrender the behavior
this repository exists to control.

Therefore this design mirrors Codex's prebuilt platform packages while shipping
BetterCodex binaries. It does not depend on, modify, or duplicate an installed
Codex package.

## Distribution channel

GitHub Releases is the binary distribution channel. GitHub Actions and GitHub
Packages are explicitly out of scope.

Release binaries are built and uploaded from explicitly operated maintainer
machines. Publishing a release consumes no Actions minutes or Actions artifact
storage. GitHub documents release assets separately and states that releases
have no total-size or bandwidth limit, with a 2 GiB limit per asset:

<https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases#storage-and-bandwidth-quotas>

Upstream Codex likewise installs platform packages rather than compiling on the
user's machine:

- <https://developers.openai.com/codex/cli/features#getting-started>
- <https://github.com/openai/codex/blob/main/scripts/install/install.sh>
- <https://github.com/openai/codex/blob/main/codex-cli/bin/codex.js>

### Release identity

Every release corresponds to one immutable, full Git source revision.

- Tag: `bcodex-<40-lowercase-hex-source-revision>`
- Release title: `BetterCodex <12-character-short-revision>`
- Release type: non-draft and non-prerelease once complete
- Tag target: exactly the revision encoded in the tag

The latest published non-prerelease is the user update channel. Public `main`
is the development branch, not an implicit distribution event. An uncommitted,
unpushed, or unreleased commit must not trigger an update notice.

This separation is essential: users are only told to update after verified
assets exist. It also lets maintainers prepare a draft release before moving
`main` to that revision.

Published releases and their tags must not be edited. Repository-level
immutable releases should be enabled before production rollout. If a release is
bad, publish a new source revision and release rather than replacing old bytes.

## Supported assets

A complete release contains one gzip-compressed executable and one checksum
file for every supported target:

| System | Rust target | Binary asset |
| --- | --- | --- |
| Linux x86-64 | `x86_64-unknown-linux-gnu` | `bcodex-x86_64-unknown-linux-gnu.gz` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `bcodex-aarch64-unknown-linux-gnu.gz` |
| macOS Intel | `x86_64-apple-darwin` | `bcodex-x86_64-apple-darwin.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `bcodex-aarch64-apple-darwin.gz` |

The checksum asset appends `.sha256` to the binary asset name. Its complete
contents are one line in this format:

```text
<64-lowercase-hex-sha256>  <binary-asset-name>
```

Gzip is selected because it reduces the current binary to about 25 MB and is
available by default on supported macOS and Linux systems. Requiring npm, xz,
zstd, a package manager, or another decompressor would make the bootstrap less
portable.

A release must not be published until all four asset pairs pass their native
verification. The source fallback is a safety valve, not permission to publish
chronically incomplete releases.

## Release build contract

Each asset must be built from a clean checkout of the tag target with the
repository's lockfile and pinned Rust toolchain.

The build must set:

```text
BCODEX_SOURCE_REVISION=<tag's full source revision>
```

The resulting executable must satisfy all of the following before upload:

- `bcodex --version` exactly matches the package version;
- `bcodex --internal-source-revision` exactly matches the tag revision;
- `bcodex --internal-install-smoke` succeeds in an isolated home and working
  directory;
- every embedded system resource materializes and verifies;
- the executable is stripped of unnecessary release symbols;
- the gzip asset's SHA-256 matches its checksum file; and
- a clean native test machine can execute the decompressed bytes.

Linux release builds must use a controlled compatibility environment rather
than the maintainer's current desktop. The initial compatibility floor is
Ubuntu 20.04 / glibc 2.31. Publication tooling must reject a Linux executable
that imports a newer `GLIBC_*` symbol, and each architecture must be smoke-tested
on that floor. If the current V8 archive or another dependency cannot satisfy
the floor, the corresponding prebuilt asset remains disabled and the issue is
reported; silently publishing a host-specific binary is forbidden.

macOS release builds set `MACOSX_DEPLOYMENT_TARGET=12.0` and are tested on the
oldest supported macOS version. Production assets should use Developer ID
signing and notarization when the required Apple credentials are available.
The publisher must always run `codesign --verify` for a signed asset and must
never silently alter a binary after its digest is recorded.

## Manual publication protocol

No repository workflow automatically compiles or publishes releases. A
checked-in maintainer command may package and upload the native target on which
it is run, but invocation is explicit.

The publication sequence is:

1. Select a clean candidate commit and run all repository checks.
2. Create the annotated `bcodex-<revision>` tag at exactly that commit.
3. Create a draft GitHub Release for the tag.
4. On each trusted build host, check out the tag and run the native release
   packaging command.
5. Have that command verify the binary, create the `.gz` and `.sha256` files,
   and upload only its target's pair to the draft release.
6. Independently download each pair from the draft or staging output and verify
   its digest and target-specific smoke-test evidence.
7. Confirm all four target pairs are present and uniquely named.
8. Publish the release as a non-prerelease.
9. Fast-forward or merge public `main` to include the released commit.

The packaging command must refuse:

- a dirty checkout;
- a revision other than the checked-out release tag;
- a package build without the embedded full revision;
- an unsupported host or requested target;
- an existing published release asset;
- failed unit, smoke, compatibility, signing, or checksum checks; and
- a tag whose target differs from `HEAD`.

Uploading with `--clobber` is allowed only while repairing an unpublished draft.
It is forbidden after publication.

## Update discovery

Installed release builds perform one failure-silent background lookup after the
TUI's first frame. The lookup requests the latest public BetterCodex release,
validates the tag format, and extracts the full revision from the tag.

- If it matches the executable's embedded revision, no notice is shown.
- If it differs, the TUI shows the current and available short revisions and
  tells the user to run `bcodex update` in another terminal.
- Network, malformed-response, draft, prerelease, and invalid-tag failures stay
  silent and are retried on the next launch.
- `BCODEX_SKIP_UPDATE_CHECK=1` continues to disable the background lookup.

`bcodex update` performs the same release lookup synchronously. An already
current update exits after that single metadata request and performs no asset,
installer, filesystem-cache, Rust, or compiler work.

## Prebuilt install and update protocol

The one-line bootstrap fetches the small canonical installer from public
`main`. The explicit updater fetches the installer from the exact released
source revision it just resolved and passes both the release tag and revision
to it. This avoids a duplicate initial metadata lookup and keeps installer
behavior paired with the release source.

For the selected release, the installer must:

1. Validate the repository, tag, revision, operating system, and architecture.
2. Derive the exact target and deterministic asset names.
3. Create a private temporary directory and acquire the existing install lock.
4. Download the target's `.sha256` file with bounded retries and timeouts.
5. Parse exactly one digest for the expected asset; reject extra paths or an
   invalid digest.
6. Download the `.gz` asset to a partial file with bounded retries.
7. Verify the compressed asset's SHA-256 before decompression.
8. Decompress directly into a staged executable in the destination directory.
9. Set mode `0755` and reject symlinks or non-regular destination paths.
10. Verify version, full embedded revision, and the complete install smoke test.
11. Re-resolve the latest release. If a newer release appeared, discard the
    stage and retry that release, up to three release attempts.
12. Compare the staged bytes where applicable and atomically rename the stage
    over the installed `bcodex`.
13. Verify the installed executable once more.
14. Remove the downloaded asset, checksum, temporary tree, lock record, and
    stale stage files on success or failure.

The existing executable remains untouched until every check succeeds. A failed
download, digest, decompression, compatibility check, smoke test, race check, or
copy leaves it runnable. A currently running TUI continues executing its old
in-memory image and tells the user to restart after replacement.

The prebuilt path must check only tools it actually needs: `/bin/sh`, `curl`, a
SHA-256 implementation, `gzip`, and basic POSIX filesystem tools. It must not
check for or install rustup, Cargo, or a C compiler.

## Source-build fallback

The source path remains available for three cases:

- no BetterCodex release has been published yet;
- the native asset is genuinely absent for a supported release; or
- a verified prebuilt executable cannot run on the local operating-system
  baseline.

A transient metadata or asset download failure must be retried and then
reported. It must not unexpectedly turn into a multi-gigabyte local build.

Before fallback, the installer prints a conspicuous explanation that a prebuilt
asset was unavailable and that the local build requires Rust, a native C
toolchain, several gigabytes of free space, and substantially more time. The
user can rerun after installing those prerequisites.

The fallback downloads the immutable released source revision, uses `Cargo.lock`
and `rust-toolchain.toml`, and runs the existing binary and resource smoke tests.
It may resolve public `main` only when bootstrapping a repository that has no
release at all.

Fallback builds retain one compatible compiled-dependency generation at:

```text
${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex/build/<native-target>/target
```

The generation identity includes the native target and hashes of the pinned
toolchain, `Cargo.toml`, `Cargo.lock`, and `scripts/cargo-with-v8.sh`. An identity
change removes the old generation before compiling. BetterCodex-owned binaries,
fingerprints, and incremental output are removed before and after each build;
registry and Git dependency artifacts remain reusable. Source and compiler
scratch space always remain temporary.

This cache makes repeated source fallbacks tolerable, but it is not the normal
user update architecture.

## Cache migration and retained footprint

After a successful prebuilt installation, the installer may remove only its
known source-updater caches:

```text
${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex/build
${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex/cargo
${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex/tmp
```

Removal happens after the new executable is verified and installed. It never
follows symlinks and never removes credentials, sessions, settings, arbitrary
cache siblings, Rust toolchains, or native compiler installations. The
`rusty-v8-*` cache is retained because development checkouts intentionally
share it; it is small relative to Cargo source and target caches and can be
deleted manually.

The normal retained user footprint is therefore the installed executable plus
BetterCodex user data, not a Rust source or compilation cache. The compressed
release asset is temporary and is not retained after a successful update.

## Failure and downgrade behavior

- A missing or invalid latest-release response does not modify the install.
- A missing checksum or asset does not install unverified bytes.
- A checksum mismatch is fatal and must name the expected asset.
- A staged binary with the wrong source revision is fatal even if its version
  string matches.
- A runtime compatibility failure may offer or enter the documented source
  fallback; it never replaces the working executable first.
- A release published while an update is in progress causes a bounded retry.
- Continuously changing releases stop after three attempts.
- Automatic downgrade is forbidden. An explicitly requested older tag may be a
  future maintainer feature but is not part of `bcodex update`.
- Interrupted installs leave a cleanup record so the next invocation can
  remove orphaned temporary and staged files.

## Security model

GitHub repository write access is already the authority for BetterCodex source
and installer changes. Release assets use the same authority. TLS authenticates
downloads; immutable release tags bind them to one source revision; SHA-256
detects corruption; the embedded revision and install smoke test prevent a
wrong or incomplete binary from being activated.

Checksums hosted beside an asset are not a substitute for an independent
signature if the GitHub account is compromised. A later hardening phase may add
an offline release signing key, but it must not introduce a package manager,
daemon, or heavyweight verifier into the install path. macOS platform signing
is complementary to the release digest.

The installer treats every downloaded filename and response as untrusted. It
constructs asset names locally, validates full revisions and repository names,
uses no server-provided filesystem paths, refuses symlink destinations, bounds
response and asset sizes, and never evaluates downloaded metadata as shell.

## Test requirements

Hermetic installer tests must cover:

- all four operating-system and architecture mappings;
- an already-current release with no binary download;
- a successful prebuilt first install and update without `cargo`, `rustup`, or
  `cc` on `PATH`;
- exact release tag, asset URL, checksum URL, and embedded revision propagation;
- transient metadata, checksum, and asset retries;
- malformed tags and checksums;
- missing, empty, oversized, truncated, and corrupt gzip assets;
- checksum mismatch;
- wrong version, wrong embedded revision, and failed resource smoke test;
- destination symlink and non-regular-file rejection;
- preservation of an existing binary on every failure;
- release advancement during verification and the three-attempt bound;
- source fallback only for a confirmed missing/incompatible asset;
- no fallback after ordinary network failure;
- bounded source-cache reuse and identity invalidation;
- post-prebuilt cleanup of only known source-updater caches; and
- orphaned lock, stage, and temporary-tree recovery.

Native release qualification must additionally test each asset on its minimum
supported operating-system baseline and run the actual compressed download,
digest, decompression, smoke, and atomic-install path.

## Acceptance criteria

The prebuilt path is complete when all of the following are true:

- `bcodex update` compiles zero packages for every published supported target;
- users do not need Rust, Cargo, a C compiler, npm, Homebrew, or GitHub CLI;
- an unchanged installation exits after one bounded release lookup;
- a changed installation downloads only the checksum and one native compressed
  executable, expected to be tens rather than hundreds or thousands of MB;
- no GitHub Actions workflow or GitHub Packages dependency is introduced;
- release bytes are tied to an immutable source revision and verified before
  replacement;
- failed updates preserve the previous executable;
- temporary update files are removed on success and recoverable after a crash;
- old source-build caches can be reclaimed after a successful prebuilt update;
  and
- source fallback remains correct but is clearly identified as the exceptional
  slow path.

The operational goal on a typical broadband connection is an update measured
in seconds, dominated by downloading and verifying an approximately 25 MB
archive. Any normal update that invokes Cargo fails this specification.

## Rollout

1. Implement release-aware discovery and the prebuilt installer path behind
   hermetic tests.
2. Add the explicit native packaging/upload command; do not add Actions.
3. Qualify Linux binaries against the glibc floor and macOS binaries against
   the deployment target.
4. Prepare all four assets in a draft release for one candidate revision.
5. Publish that immutable release and move public `main` to include it.
6. Use the existing source installer once only where needed to migrate older
   binaries to release-aware update discovery.
7. After the first successful real update on every target, make this document
   accepted and update `README.md`, `docs/install.md`, CLI help, and migration
guidance to describe releases as the distribution channel.
