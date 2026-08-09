# Installing and building bettercodex

bettercodex is one Rust package and one `bcodex` binary. Public installations
track the exact current commit on public `main`. Cargo package versions are
display metadata; they do not decide whether an installation is current.

## Supported and preview systems

| Operating system | Architectures | Status |
| --- | --- | --- |
| macOS 12+ | Apple Silicon and Intel | Supported |
| Linux with glibc 2.31+ | ARM64 and x86-64 | Supported |
| Windows 11 | x86-64 | Developer preview |

Native Windows 10 version 1809 or newer has the required ConPTY API but remains
best effort. The native Windows port remains a developer preview until its
native workflow and interactive Windows Terminal/VS Code matrix are recorded.
WSL runs the Linux binary and follows the Linux instructions, not the native
Windows flow.

A public install requires several gigabytes of free space and network access to
GitHub and the official Rust download servers. The installer supplies its own
[rustup](https://rustup.rs/) and pinned Rust toolchain when necessary. Unix
installation also requires `curl`. If native C/C++ build tools are missing on
Linux, the installer installs the distribution's development-tool package;
privilege escalation may request the operator's password. On a new Mac it
starts Apple's Xcode Command Line Tools installer. Finish the macOS system
dialog and rerun the command once.

Native Windows additionally requires PowerShell 5.1 or PowerShell 7, the
Windows `tar.exe`, and Visual Studio 2022 Build Tools with **Desktop development
with C++** and a Windows 10 or 11 SDK. Install those prerequisites from
[Microsoft's C++ Build Tools page](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
before running the source installer. The first local release build can use
roughly 6–10 GiB persistently for Rust, dependency, V8, and compiled-artifact
caches. For a cold build, the installer reserves 8 GiB of cache headroom,
2 GiB of temporary-build headroom, and 256 MiB for installation; when those
paths share a volume, at least 10.25 GiB must be free. The installed product
itself remains one `bcodex.exe`. The installer aggregates this preflight by
volume before starting the long source build.

## Install

### macOS and Linux

Run the repository's canonical [`INSTALL_COMMAND.txt`](../INSTALL_COMMAND.txt):

```sh
curl -fsSL https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.sh | sh
```

The bootstrap downloads `scripts/install.sh` from public `main`. The installer
then:

1. Resolves `refs/heads/main` once to a full 40-character commit ID.
2. Downloads the immutable source archive for exactly that commit.
3. Bootstraps missing native build tools, rustup, and the pinned Rust toolchain.
   A rustup just installed at `$HOME/.cargo/bin` is detected immediately even
   when the current shell has not reloaded `PATH`.
4. Uses the checked-in lockfile and pinned Rust toolchain.
5. Hashes release-relevant source bytes, then reuses Cargo's native-target cache
   and incrementally builds only changed bettercodex code and dependencies.
6. Stamps the exact commit into a staged copy without invalidating reusable
   compiler output, then reapplies and verifies the required ad-hoc code
   signature on macOS.
7. Checks its version and embedded revision, initializes V8, and materializes
   every embedded system resource in isolated directories.
8. Atomically renames the verified stage over the installed command.

The selected commit does not change if `main` advances while compilation is in
progress. The next launch or explicit update discovers the newer commit.

By default, the command is installed at `$HOME/.local/bin/bcodex`. The installer
adds a managed PATH block to the appropriate shell profile when necessary and
reports any older `bcodex` that appears earlier on `PATH`. Set the absolute
`BCODEX_INSTALL_DIR` environment variable to choose another binary directory.

### Native Windows

In Windows PowerShell 5.1:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm 'https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.ps1' | iex"
```

PowerShell 7 users can run the equivalent command with `pwsh`. This invocation
does not change the machine-wide execution policy. The installer pins public
`main`, downloads that exact source archive, initializes the installed Visual
Studio build environment, obtains the pinned Rust toolchain and verified
sandboxed V8 pair, builds and smoke-tests `bcodex.exe`, then commits only the
verified candidate.

The default Windows command directory is
`%LOCALAPPDATA%\Programs\bettercodex\bin`; reusable build state is under
`%LOCALAPPDATA%\bettercodex\cache`. Set an absolute `BCODEX_INSTALL_DIR` or
`BCODEX_CACHE_DIR` before invoking the script to override either location. The
installer updates the current process PATH and the case-insensitive per-user
PATH without duplicating entries. Open a new terminal after first installation.

After installation, open a new terminal when requested, then run:

```sh
bcodex login
bcodex
```

`CODEX_HOME` and `BCODEX_HOME` override the credential and bettercodex state
directories on every platform. Without overrides, Unix uses `$HOME/.codex` and
`$HOME/.bcodex`; native Windows uses `%USERPROFILE%\.codex` and
`%USERPROFILE%\.bcodex`. Installing or updating does not remove either
directory.

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
and Cargo's fine-grained compilation state, performs all verification again,
and atomically replaces the command. A manifest or lockfile edit recompiles
only affected artifacts; it never erases the whole target first. On Windows,
where the running executable cannot be replaced, the verified installer starts
a bounded PowerShell finalizer. The updater exits, the finalizer verifies that
exact process identity, replaces `bcodex.exe` with rollback protection, and
verifies the visible revision. Restart a running TUI to use the new binary.

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

When a cache home is available, the installer retains reusable dependency
downloads and one native-target Cargo cache under
`${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex`. If it must supply rustup or the
pinned toolchain, those stay in the same cache. An existing user toolchain is
reused when it already provides the pinned Rust release. Cargo fingerprints
compiler, profile, manifest, lockfile, feature, build-script, dependency, and
source changes at artifact granularity. The updater also makes a SHA-256 hash
of the release inputs a compiler input, avoiding Cargo's mtime ambiguity across
newly extracted source trees. The staged binary must match that hash before it
can be stamped. The updater preserves Cargo artifacts and enables incremental
compilation for the bettercodex package; registry dependencies do not gain
incremental copies. Source archives, extracted source, compiler scratch space,
and stage files remain disposable.

The installer refuses symlinked destinations and never follows cache symlinks
while cleaning. A lock rejects concurrent installs and records temporary state
so a later invocation can recover an orphan left by an untrappable crash. Any
failed download, extraction, build, smoke test, copy, or verification leaves an
existing installed binary untouched.

On Windows the same checks cover junctions and other reparse points. Candidate,
backup, and transaction files stay under one uniquely named install-owned
directory beside `bcodex.exe`; a later invocation recovers only a validated
transaction record. Bounded retries cover transient sharing violations, while
permission and reparse failures stop immediately.

Without `HOME` or `XDG_CACHE_HOME`, rustup, the pinned Rust toolchain, dependency
downloads, and build output stay inside the disposable installation tree and
are removed afterward.

## Build from a checkout

Use the checked-in wrapper so the V8 archive and binding match the crate:

```sh
./scripts/cargo-with-v8.sh build --locked
./scripts/cargo-with-v8.sh run --bin bcodex
```

On native Windows, use the target-specific wrapper instead:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/cargo-with-v8.ps1 build --locked
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/cargo-with-v8.ps1 run --bin bcodex
```

Development builds intentionally have no embedded distribution revision and do
not perform update checks. To install a checkout manually:

```sh
./scripts/cargo-with-v8.sh install --locked --path . --force \
  --root "$HOME/.local"
```

For a manually built native Windows checkout, use the release executable under
`target\release\bcodex.exe`; use `scripts/install.ps1` for an installation that
tracks public `main` and configures PATH.

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

Native Windows validation is defined in [`.github/workflows/windows.yml`](../.github/workflows/windows.yml):
format and PowerShell syntax checks, `cargo check --tests`, the native test
suite, warning-denied Clippy, PowerShell installer transaction tests, a release
smoke test, and an exact-revision install/no-op cycle. Run the focused installer
tests directly with:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/install_windows_tests.ps1
```

The Windows status must not be promoted from developer preview until that
workflow and the primary interactive terminal matrix pass on native Windows.

See [the development workflow](../progressive_disclosure/development.md) for
the complete contribution procedure.
