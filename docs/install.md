# Installing bettercodex

bettercodex is distributed directly from the integrated `main` branch of a
private GitHub repository. The installer resolves `main` to one immutable Git
commit and compiles that source natively on each computer. There are no prebuilt
GitHub Release binaries and no hosted release builds.

## Supported systems

| Operating system | Architectures |
| --- | --- |
| macOS | Apple Silicon and Intel |
| Linux | ARM64 and x86-64 |

Windows is not supported.

## Prerequisites

Each computer needs:

- access to `ummay0432/bettercodex` through its own GitHub account;
- the authenticated [GitHub CLI](https://cli.github.com/);
- Python 3;
- Cargo and Rust installed through [rustup](https://rustup.rs/); and
- a native C compiler and the normal development libraries for that operating
  system.

On macOS, the Xcode Command Line Tools provide the native compiler. On Linux,
the distribution's C build toolchain, `pkg-config`, and OpenSSL development
package may be needed. The checked-in `rust-toolchain.toml` selects Rust 1.95.0.

## First install

1. Accept the repository invitation while signed into the invited GitHub
   account.
2. Authenticate the GitHub CLI:

   ```sh
   gh auth login
   ```

3. Run the installer:

   ```sh
   gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | sh
   ```

4. Open a new terminal and sign in with the computer owner's ChatGPT account:

   ```sh
   bcodex login
   ```

5. Enter a project directory and run `bcodex`.

The ChatGPT credential cache stays in `$CODEX_HOME/auth.json`, or
`$HOME/.codex/auth.json` when `CODEX_HOME` is unset. Bettercodex settings and
saved sessions stay under `$HOME/.bcodex`. Both remain local to that computer.

## What the installer does

The installer:

1. resolves private repository `main` to an exact Git commit and downloads that
   immutable source snapshot;
2. embeds the resolved source revision in the compiled binary;
3. retries from the newer commit if `main` advances while the local build is
   running;
4. downloads and SHA-256 verifies the pinned sandbox-enabled V8 archive and
   matching Rust bindings for the local Rust host;
5. builds the locked source in release mode using a persistent Cargo cache;
6. requires the result to report the expected `bcodex X.Y.Z`;
7. starts V8 and ICU, materializes the embedded skills and evaluator manifest,
   and checks the generated tool context; and
8. atomically replaces the installed binary only after every prior step passes
   and `main` still resolves to the embedded revision.

The default binary is `$HOME/.local/bin/bcodex`. The default build cache is
`$HOME/.cache/bettercodex/build`, or the corresponding location under
`$XDG_CACHE_HOME`. The first build is substantial. Later updates reuse Cargo,
Git dependency, and V8 caches, though bettercodex itself is always rebuilt from
the selected immutable source snapshot.

## Updating

After the TUI renders, an installed build compares its embedded source revision
with the current private `main` commit in the background. Startup does not wait
for the request. When they differ, the TUI shows `Update available` and the
single command to run in another terminal:

```sh
bcodex update
```

The running TUI keeps its old in-memory code until it is restarted. Failed
background checks stay silent and are retried on the next launch; set
`BCODEX_SKIP_UPDATE_CHECK=1` to disable them.

Version 0.1.2 is the one-time bridge from version-based checks to source
revision checks. Existing 0.1.1 devices discover that bridge through its stable
tag; after installing it, later integrated changes need no version bump or tag.

Version 0.1.0 embedded the retired prebuilt-release installer. It cannot learn
the source-build flow retroactively. To move an existing 0.1.0 installation to
the current update channel, rerun the original installer command once:

```sh
gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | sh
```

After that one-time bootstrap, `bcodex update` uses local source builds and
tracks the integrated source revision.

To install a specific stable tag:

```sh
gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | BCODEX_RELEASE=v0.1.2 sh
```

To use custom binary and build-cache directories:

```sh
gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | \
  BCODEX_INSTALL_DIR="$HOME/bin" \
  BCODEX_BUILD_DIR="$HOME/.cache/bettercodex-build" \
  sh
```

`bcodex update` defaults to the directory containing the running binary, so a
custom installation stays in place. `BCODEX_INSTALL_DIR` can override it.

## What a successful build proves

Rust dependencies, prompts, the installer, system skills, the evaluator
manifest, V8, and ICU data are compiled or embedded into one executable. A
successful native build catches missing compile-time components for that
computer. The installer's post-build smoke test then starts V8 and ICU and
checks that the embedded resources can be materialized, before replacing the
old binary.

It does not prove that another operating system or CPU can compile the same
source. Under this distribution model each device performs that proof itself.
The installed program still expects host facilities where relevant: network
access and authenticated `gh` for private updates, `git` for repository work,
and `tmux` when automatic terminal management is enabled.

## Privacy boundary

The private repository is the invite gate. Each operator authenticates as
themselves, so the install command contains no shared token. Removing a
collaborator blocks future source downloads, but cannot erase source or binaries
already downloaded.

This personal GitHub repository gives collaborators write access. If operators
should have read-only access, transfer it to an organization and grant the Read
role. Never share GitHub tokens, `$HOME/.codex/auth.json`, or `$HOME/.bcodex`.

## Source checkpoints

Normal device updates track integrated `main` and do not require a package
version change or source tag. Stable `vX.Y.Z` tags remain supported as explicit,
immutable checkpoints. When intentionally publishing one, its package version
must match. Validate a clean `main`, then push the annotated tag explicitly:

```sh
git tag -a v0.1.2 -m "Release bettercodex 0.1.2"
git push origin refs/tags/v0.1.2:refs/tags/v0.1.2
```

Do not move a published tag. There is no GitHub Release object or artifact
matrix to maintain.

Contributors should build through the checked-in helper so the pinned V8 pair
is selected and verified correctly:

```sh
./scripts/dev.py cargo build --release --locked
```

The helper prints the target directory containing `release/bcodex`.
