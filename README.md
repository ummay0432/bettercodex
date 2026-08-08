# bettercodex

bettercodex is a focused public port of the OpenAI Codex CLI for people who
want a more proactive coding agent. It is an unofficial community project, not
an OpenAI product.

BetterCodex runs commands and applies patches with your user account's full
permissions. It does not provide Codex's sandbox. Use it only on machines and
repositories where you accept that trust model.

## Install

On macOS 12+ or Linux with glibc 2.31+, copy and run this command:

```sh
( set -eu; command -v curl >/dev/null 2>&1 || { printf '%s\n' 'bettercodex installer: curl is required' >&2; exit 1; }; bcodex_bootstrap="$(mktemp)"; trap 'rm -f "$bcodex_bootstrap"' 0; trap 'exit 1' 1 2 15; if ! curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location --connect-timeout 10 --max-time 30 --max-filesize 1048576 --user-agent bettercodex https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.sh >"$bcodex_bootstrap"; then printf '%s\n' 'bettercodex installer: could not fetch the public installer' >&2; exit 1; fi; [ "$(sed -n '1p' "$bcodex_bootstrap")" = '#!/bin/sh' ] || { printf '%s\n' 'bettercodex installer: GitHub returned an invalid installer' >&2; exit 1; }; /bin/sh "$bcodex_bootstrap" )
```

[`INSTALL_COMMAND.txt`](INSTALL_COMMAND.txt) contains the same one-line command.
When a compatible native release exists, it downloads the roughly 22 MB
compressed executable, verifies its checksum, version, source revision, V8
runtime, and embedded resources, then atomically installs it. That path needs
`curl` but no npm, GitHub login, Rust, compiler, or local source build.

The first complete four-platform native release has not been published yet.
Until it is, the installer clearly enters its source fallback, which requires
[rustup](https://rustup.rs/), a native C toolchain, several gigabytes of free
space, and more time. The same command automatically starts using native assets
after the release is available.

Then open a new terminal, sign in with a ChatGPT account that has Codex access,
and launch bettercodex from a project directory:

```sh
bcodex login
bcodex
```

Installed builds compare their embedded source revision with the latest
published release after the TUI renders. When an update is available, run
`bcodex update` in another terminal. It streams a compact patch from the
installed revision when available, otherwise a roughly 17 MB full native
update, and atomically installs the verified executable. It compiles zero Cargo
packages, never enters source fallback, and retains no Rust build cache. A
bounded source build exists only in the bootstrap installer when no compatible
first-install asset exists.

Only complete, non-prerelease GitHub Releases are distributed. Public branches
and tags remain visible but do not trigger user updates. Missing or edited
embedded system files are integrity-checked and atomically repaired when the
updated binary launches.

For a task that merits evaluator-backed iteration, use `/loop <task>` in the
TUI or include `$loop` anywhere in an interactive or non-interactive prompt.
The default runs one evaluator session followed by three fresh working
sessions; see [`docs/slash_commands.md`](docs/slash_commands.md#quality-loop)
for counts, progress, restoration, and repository-local evidence.

For an active engineering review and evidence-backed refactoring, use
`/review <target>` in the TUI or invoke `$review <target>` in any prompt. The
workflow develops a deep understanding of the target, applies explicit quality
criteria, and acts on clear net improvements. The agent can also select this
workflow proactively during implementation work; see
[`docs/slash_commands.md`](docs/slash_commands.md#engineering-review).

See [`docs/install.md`](docs/install.md) for supported platforms, verification,
fallback, and migration details. [`spec-install.md`](spec-install.md) defines
the release and updater contract.
