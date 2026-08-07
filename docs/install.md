# Installing bettercodex

bettercodex is distributed as private, prebuilt GitHub Release binaries. A
friend needs repository access, but does not need Rust, npm, or a copy of the
source tree.

## Supported systems

The release workflow builds native binaries for:

| Operating system | Architectures | Baseline |
| --- | --- | --- |
| macOS | Apple Silicon and Intel | macOS 12 or newer |
| Linux | ARM64 and x86-64 | Ubuntu 22.04 or a compatible newer distribution |

Windows is not supported. The installer writes `bcodex` to
`$HOME/.local/bin` and adds that directory to the appropriate shell profile
when necessary.

## Friend setup

1. The maintainer sends a read-only GitHub repository invitation. Accept it
   while signed in to the GitHub account that should retain access.
2. Install the [GitHub CLI](https://github.com/cli/cli#installation), then sign
   in:

   ```sh
   gh auth login
   ```

3. Install bettercodex:

   ```sh
   gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | sh
   ```

4. Open a new terminal, enter a project directory, and run:

   ```sh
   bcodex
   ```

5. Sign in with your own ChatGPT account when prompted. Authentication,
   settings, and saved sessions stay under `$HOME/.bcodex` on that computer.

Run the same installer command whenever the maintainer publishes an update.
The installer authenticates with `gh`, selects the native release asset,
verifies its SHA-256 checksum, checks the reported version, and replaces the
installed binary atomically.

To install a specific release:

```sh
gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | BCODEX_RELEASE=v0.1.0 sh
```

To use a different binary directory:

```sh
gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | BCODEX_INSTALL_DIR="$HOME/bin" sh
```

## Privacy boundary

The private repository and its private release assets are the invite gate.
Each friend authenticates as themselves, so the install command contains no
shared token to leak. Give friends the read role; they do not need write
access.

Removing a collaborator blocks future source and release downloads. It cannot
erase binaries or source that the collaborator already downloaded, so this is
private distribution rather than digital-rights management. Friends should
never share GitHub tokens, ChatGPT credentials, or `$HOME/.bcodex` contents.

## Maintainer setup

GitHub Actions must be enabled for this private repository. Private-repository
runner minutes count against the repository owner's GitHub plan, and macOS
jobs consume more billed minutes than Linux jobs.

Invite a friend with read-only access by replacing `FRIEND` with their GitHub
username:

```sh
gh api --method PUT repos/ummay0432/bettercodex/collaborators/FRIEND -f permission=pull
```

GitHub emails the invitation. The friend must accept it before the installer
can read the repository or its releases.

## Publishing a release

The package version and release tag must match. From a clean, validated `main`
branch that has already been pushed to `origin`:

```sh
git tag -a v0.1.0 -m "Release 0.1.0"
git push origin v0.1.0
```

The `Release` workflow builds and smoke-tests all four platform binaries,
creates `SHA256SUMS`, and publishes or repairs the matching private GitHub
Release. Follow it with:

```sh
gh run watch --repo ummay0432/bettercodex
```

For a later release, update the `version` in `Cargo.toml`, refresh `Cargo.lock`,
validate the change, merge and push it, then create the matching tag.

## Building from source

Contributors should use the checked-in development helper so the pinned V8
artifacts are downloaded and verified correctly:

```sh
./scripts/dev.py cargo build --release
```

The resulting binary is under the target directory printed by the helper.
