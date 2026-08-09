# bettercodex installation and update specification

Status: implemented

This document is the normative contract for public bettercodex installation
and updates. Public `main`, identified by its exact Git commit, is the only
freshness channel. Package versions, tags, drafts, and published GitHub
Releases are outside the update decision.

## Invariants

- A distribution binary is current exactly when its embedded full source
  revision equals the commit at `refs/heads/main`.
- Every changed public `main` commit is eligible immediately; no version bump or
  release publication is required.
- The package version is user-facing metadata only and may remain unchanged
  across any number of source revisions.
- A build installs the source commit selected before its archive download
  begins. The explicit updater fetches its installer from that same commit.
- No unverified candidate replaces an existing command.
- The source-selection, build-input, staging, smoke, and revision-verification
  contract is identical on macOS ARM64, macOS x86-64, Linux ARM64, Linux
  x86-64, and Windows x86-64. Commit mechanics remain target-native.

## Revision discovery

The installer and updater query:

```text
GET https://api.github.com/repos/<owner>/<repository>/git/ref/heads/main
```

The response is valid only when all of the following hold:

- `ref` is exactly `refs/heads/main`;
- `object.type` is exactly `commit`; and
- `object.sha` is exactly 40 hexadecimal characters.

The repository name must contain exactly one slash and only ASCII letters,
digits, `_`, `-`, or `.` in each component. Requests use HTTPS-only redirects,
bounded connection and overall timeouts, response-size limits, compression,
and retries where appropriate.

Background lookup failures are silent. Explicit install and update failures are
reported and never interpreted as evidence that the current binary is fresh.

## Bootstrap installation

The canonical Unix command fetches `scripts/install.sh`; the native Windows
command fetches `scripts/install.ps1`. Both come from public `main`. Unless
`BCODEX_INSTALL_REVISION` already supplies a valid full revision, the selected
script resolves public `main` once. It downloads only the codeload archive for
the selected immutable commit and must not consult package registries, tags, or
GitHub Releases to select a version.

The source installation must:

1. Require a checked-in `Cargo.lock`, pinned `rust-toolchain.toml`, target-native
   Cargo/V8 wrapper, rustup, and a working native C compiler. Windows requires
   the x64 MSVC C++ workload and Windows SDK and may initialize its installed
   Visual Studio developer environment automatically.
2. Hash release-relevant source contents. When no target-native cached or
   installed binary proves that same complete hash, build with `--release
   --locked --bin bcodex`, Cargo incremental compilation enabled, that hash as
   a tracked compiler input, and no source-revision input that would invalidate
   compiler output. If SHA-256 is unavailable, use a revision-specific
   freshness key.
3. Require `bcodex --version` to match the package metadata in that source.
4. Require the selected binary's tracked release-input hash to match, then use
   its internal staging helper to copy itself beside the destination and
   replace its unique fixed-size revision marker with the selected commit. On
   macOS, ad-hoc sign the modified Mach-O and verify that signature before
   executing it.
5. Require staged `bcodex --internal-source-revision` to equal the selected
   commit.
6. Run staged `bcodex --internal-install-smoke` with isolated user and
   application homes, covering V8 startup and every embedded resource.
7. Atomically rename the verified staged bytes over the destination. When a
   running Windows executable prevents replacement, start a bounded finalizer
   that holds the exact updater process identity, waits for its exit, rechecks
   the candidate digest, moves the old executable to an install-owned backup,
   commits the candidate, verifies it, and restores the backup on failure.
8. Verify the installed version and revision again before reporting success.

The Unix destination defaults to `$HOME/.local/bin`; the Windows destination
defaults to `%LOCALAPPDATA%\Programs\bettercodex\bin`. Either may be overridden
only with an absolute `BCODEX_INSTALL_DIR`. Symlinked destinations, Windows
junctions, and other reparse-point redirections are rejected.

## Explicit update

`bcodex update` requires a valid embedded source revision, resolves public
`main`, and exits successfully when the revisions match. If they differ, it:

1. Fetches the target-native `scripts/install.sh` or `scripts/install.ps1` from
   the resolved commit, not from a moving branch.
2. Requires a non-empty, bounded response with the target-native shell marker.
3. On Unix, runs `/bin/sh -s`. On Windows, writes the bounded verified response
   to a private temporary file and invokes `powershell.exe -NoProfile
   -ExecutionPolicy Bypass -File`; it does not route PowerShell through
   `cmd.exe` or interpolate a command string. Both receive the running binary's
   directory as `BCODEX_INSTALL_DIR`, the selected commit as
   `BCODEX_INSTALL_REVISION`, and the validated repository as
   `BCODEX_REPOSITORY`. Windows also receives the updater PID so the installer
   can bind deferred replacement to that process's start identity.
4. Lets the installer perform the complete source-install and replacement
   contract above.

The updater clears obsolete release-selection environment variables before
starting the installer. It never compares semantic versions and never treats a
draft or published release as update state.

## Background notification

Release builds check once after the initial TUI frame. Development builds,
builds without a valid embedded revision, and processes with
`BCODEX_SKIP_UPDATE_CHECK` set do not check. A valid different revision creates
an update notice containing the current and latest short commit IDs plus the
explicit update command. Equal, malformed, failed, or timed-out responses do
not create a notice.

## Cache and resource bounds

The installer may retain dependency downloads and one native-target Cargo build
cache below `${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex` on Unix or
`%LOCALAPPDATA%\bettercodex\cache` on Windows. Cargo owns
fine-grained invalidation for compiler, profile, manifest, lockfile, feature,
build-script, dependency, and source changes. The installer must not discard the
target merely because the manifest, lockfile, or wrapper changed. A content hash
of release inputs must independently prevent stale mtime-based source reuse.

The native cache retains dependency artifacts, the unstamped bettercodex build,
fingerprints, and incremental bettercodex state. Source archives, extracted
source, temporary Rust toolchains, compiler scratch, and staged executables are
disposable. A target-native installer may reuse a cached unstamped or currently
installed binary without invoking Cargo only after the installer verifies its
package version and its internal staging helper proves that the complete
release-input hash matches the selected source. Cleanup must run after success
and failure without following
symlinks or reparse points. If no cache home exists, all downloads and build
output are disposable.

Network payloads and metadata have explicit maximum sizes. Compilation happens
once for the selected revision even if `main` changes during the build. A later
launch detects any newer revision.

## Failure behavior

- Invalid repository or revision input stops before network or build work.
- Exhausted revision or archive retries preserve the installed command.
- Missing prerequisites produce an actionable platform-specific error.
- Failed extraction, compilation, embedded-revision verification, runtime smoke
  test, staging, or final verification preserves the installed command.
- An active install lock rejects concurrent mutation.
- A stale transaction may remove only its validated temporary tree, candidate,
  and backup. On Windows, a retained backup restores that transaction's
  previous verified command before cleanup, replacing an interrupted candidate
  if necessary.
- PATH setup failure does not invalidate an otherwise verified installation;
  the installer prints the manual action.

## Required regression coverage

Tests must cover exact main-ref parsing, same and different revision checks,
failure-silent background behavior, immutable installer/source URLs, pinned
environment propagation, all four host mappings, transient and exhausted
network failures, prerequisite diagnostics, granular cache reuse across
same-metadata manifest and lockfile changes, build-input verification,
revision-marker staging,
symlink refusal, stale and active locks, atomic preservation on every failed
verification, moving-`main` behavior, PATH handling, and cleanup with and
without persistent cache homes. Native Windows coverage additionally requires
PowerShell syntax, V8 checksum, `.exe` staging, reparse refusal, exact-process
deferred replacement, sharing-violation retry, rollback, release smoke, and
same-revision no-op tests.
