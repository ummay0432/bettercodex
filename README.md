# bettercodex

bettercodex is a focused, private port of the OpenAI Codex CLI for a small
group of trusted operators.

## Install

Repository access is the invite gate. After accepting an invitation and
signing in with the [GitHub CLI](https://cli.github.com/), run:

```sh
gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | sh
```

Then open a new terminal, sign in with your own ChatGPT account, and launch
bettercodex from a project directory:

```sh
bcodex login
bcodex
```

Interactive sessions check for a newer private release in the background after
the TUI is ready. When an update is available, install it from another terminal:

```sh
bcodex update
```

See [`docs/install.md`](docs/install.md) for first-time GitHub setup, supported
platforms, update behavior, privacy boundaries, and release instructions.
