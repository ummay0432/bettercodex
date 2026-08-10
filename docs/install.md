# Installing and building bettercodex

Public bettercodex installations use prebuilt binaries from GitHub Releases.
They do not download source code or require Rust, Cargo, or native build tools.

## Supported systems

| System | Architecture | Status |
| --- | --- | --- |
| macOS 12 or newer | Apple silicon | Supported |
| Ubuntu 22.04+ and Debian 12+ | x86-64 | Supported |
| Windows 11 build 22000+ | x86-64 | Developer preview |

Ubuntu and Debian share the `x86_64-unknown-linux-gnu` release binary. WSL uses
that Linux binary. Native Windows remains a developer preview until the native
automated and interactive terminal matrices in
[`SPEC-WINDOWS.md`](../SPEC-WINDOWS.md) pass.

## Install

On macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.sh | sh
```

On native Windows in PowerShell 5.1 or newer:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm 'https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.ps1' | iex"
```

Open a new terminal when requested, then run:

```sh
bcodex
```

On the first interactive launch, bettercodex asks you to sign in only when no
valid Codex credentials are available. Existing credentials remain valid across
installs and updates.

The Unix installer requires `curl`, `gzip`, and standard POSIX utilities. The
Windows installer uses built-in PowerShell and .NET functionality. Both select
the matching asset from the latest published full release, reject unexpected
sizes or binary identities, and replace the installed command only after
verification succeeds. The macOS installer also verifies the binary's code
signature. Release Actions smoke-test each binary on its native runner before
it can become a release asset.

The default command locations are:

- `$HOME/.local/bin/bcodex` on macOS and Linux; and
- `%LOCALAPPDATA%\Programs\bettercodex\bin\bcodex.exe` on Windows.

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

After the TUI renders, a distribution build performs one bounded, failure-silent
check against GitHub's latest non-draft, non-prerelease release. Set
`BCODEX_SKIP_UPDATE_CHECK=1` to disable that background check. Development builds
do not check for updates.

To update a published build, run:

```sh
bcodex update
```

The updater validates the latest release metadata and target asset, fetches the
installer from the immutable source revision encoded in that release tag, and
installs the matching prebuilt binary. It never compiles locally. Unix replaces
the binary atomically; Windows stages a verified replacement that finishes
after the running process exits.

`BCODEX_REPOSITORY` selects another `owner/repository` for development or fork
testing. `BCODEX_INSTALL_RELEASE_TAG` pins an exact asset and is reserved for the
updater and release validation.

## State directories

`CODEX_HOME` and `BCODEX_HOME` override credential and bettercodex state
directories on every platform. Without overrides, they are `$HOME/.codex` and
`$HOME/.bcodex` on Unix, or `%USERPROFILE%\.codex` and
`%USERPROFILE%\.bcodex` on Windows.

## Build from a checkout

Source compilation is a developer workflow. Use the checked-in wrapper so the
V8 archive and Rust binding match:

```sh
./scripts/cargo-with-v8.sh build --locked
./scripts/cargo-with-v8.sh run --bin bcodex
```

On native Windows:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/cargo-with-v8.ps1 build --locked
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/cargo-with-v8.ps1 run --bin bcodex
```

Development binaries intentionally have no embedded release tag, so
`bcodex update` is unavailable for them.

## Development checks

The routine local gate mirrors upstream Codex:

```sh
just fix
just fmt
just test
just clippy -- -D warnings
./scripts/cargo-with-v8.sh build --release --locked
```

The compact installer suites cover bettercodex's intentional prebuilt-release
departure:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/install_tests.py
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/install_windows_tests.ps1
```

Run the PowerShell suite and the Windows terminal matrix on native Windows.
See [the development workflow](../progressive_disclosure/development.md) for
source and artifact rules.
