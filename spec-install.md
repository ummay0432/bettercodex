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
- The normal path is identical on macOS ARM64, macOS x86-64, Linux ARM64, and
  Linux x86-64.

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

The canonical one-line command fetches `scripts/install.sh` from public `main`.
Unless `BCODEX_INSTALL_REVISION` already supplies a valid full revision, that
script resolves public `main` once. It downloads only the codeload archive for
the selected immutable commit and must not consult package registries, tags, or
GitHub Releases to select a version.

The source build must:

1. Require a checked-in `Cargo.lock`, pinned `rust-toolchain.toml`, executable
   Cargo/V8 wrapper, rustup, and a working native C compiler.
2. Build with `--release --locked --bin bcodex` and
   `BCODEX_SOURCE_REVISION=<selected-commit>`.
3. Require `bcodex --version` to match the package metadata in that source.
4. Require `bcodex --internal-source-revision` to equal the selected commit.
5. Run `bcodex --internal-install-smoke` with isolated user and application
   homes, covering V8 startup and every embedded resource.
6. Stage the exact verified bytes beside the destination, compare the copy, and
   atomically rename it over the destination.
7. Verify the installed version and revision again before reporting success.

The destination defaults to `$HOME/.local/bin` and may be overridden only with
an absolute `BCODEX_INSTALL_DIR`. Symlinked destinations are rejected.

## Explicit update

`bcodex update` requires a valid embedded source revision, resolves public
`main`, and exits successfully when the revisions match. If they differ, it:

1. Fetches `scripts/install.sh` from the resolved commit, not from a moving
   branch.
2. Requires a non-empty, bounded response beginning with `#!/bin/sh`.
3. Runs `/bin/sh -s` with the running binary's directory as
   `BCODEX_INSTALL_DIR`, the selected commit as `BCODEX_INSTALL_REVISION`, and
   the validated repository as `BCODEX_REPOSITORY`.
4. Lets the installer perform the complete source-build and replacement
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

The installer may retain dependency downloads and one compatible native-target
build cache below `${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex`. Cache identity
includes the host target and hashes of the pinned toolchain, Cargo manifest,
lockfile, and V8 wrapper. An identity change replaces the previous target.

Source archives, extracted source, temporary Rust toolchains, compiler scratch,
staged executables, and bettercodex-owned build outputs are disposable. Cleanup
must run after success and failure without following symlinks. If no cache home
exists, all downloads and build output are disposable.

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
- A stale lock may remove only the temporary tree and stage recorded by the
  installer, then retries acquisition.
- PATH setup failure does not invalidate an otherwise verified installation;
  the installer prints the manual action.

## Required regression coverage

Tests must cover exact main-ref parsing, same and different revision checks,
failure-silent background behavior, immutable installer/source URLs, pinned
environment propagation, all four host mappings, transient and exhausted
network failures, prerequisite diagnostics, cache reuse and invalidation,
symlink refusal, stale and active locks, atomic preservation on every failed
verification, moving-`main` behavior, PATH handling, and cleanup with and
without persistent cache homes.
