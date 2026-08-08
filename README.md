# bettercodex

bettercodex is a focused, private port of the OpenAI Codex CLI for a small
group of trusted operators.

## Install

Repository access is the invite gate. After accepting an invitation and
signing in with the [GitHub CLI](https://cli.github.com/), install Python 3,
Rust through rustup, and a native C toolchain. Then run:

```sh
gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | sh
```

Then open a new terminal, sign in with your own ChatGPT account, and launch
bettercodex from a project directory:

```sh
bcodex login
bcodex
```

For a task that merits evaluator-backed iteration, use `/loop <task>` in the
TUI or include `$loop` anywhere in an interactive or non-interactive prompt.
The default runs one evaluator session followed by three fresh working
sessions; see [`docs/slash_commands.md`](docs/slash_commands.md#quality-loop)
for counts, progress, restoration, and repository-local evidence.

The installer resolves the current integrated `main` revision and compiles that
immutable source snapshot for the current Mac or Linux machine. The revision is
embedded in the binary. Interactive sessions compare it with private `main` in
the background after the TUI is ready, so updates do not depend on version
bumps or release tags. When an update is available, run:

```sh
bcodex update
```

The freshly compiled runtime and embedded resources are checked before the old
binary is replaced. Build dependencies and a persistent Cargo cache are reused
on later updates.

See [`docs/install.md`](docs/install.md) for first-time GitHub setup, supported
platforms, update behavior, privacy boundaries, and release instructions.
