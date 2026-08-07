# bettercodex

bettercodex is a focused, private port of the OpenAI Codex CLI for a small
group of trusted operators.

## Install

Repository access is the invite gate. After accepting an invitation and
signing in with the [GitHub CLI](https://cli.github.com/), run:

```sh
gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | sh
```

Then open a new terminal and run:

```sh
bcodex
```

The same installer command updates an existing installation. See
[`docs/install.md`](docs/install.md) for first-time GitHub setup, supported
platforms, privacy boundaries, and release instructions.

