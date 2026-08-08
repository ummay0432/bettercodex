# bettercodex

You cannot motivate agents to be proactive by tweaking the system prompt. You
have to do it through orchestration.

Claude Code and Codex steer the model toward the smallest change that gets the
task done. Over thousands of sessions, that compounds slop and technical debt.

bettercodex is a work in progress port of
[OpenAI Codex](https://github.com/openai/codex) trying to fix this in the
harness. The plan is to use hooks, Andrej Karpathy's
[`autoresearch`](https://github.com/karpathy/autoresearch), and context efficient
looping to make the agent notice problems, do the work, and clean up after
itself.

Start with the review skill. It does not just tell you what is wrong. It digs
into a target, fixes the slop it finds, simplifies the code, and removes dead
code. The agent can also use it on its own while working.

This is early, unofficial, and not an OpenAI product.

bettercodex has no Codex sandbox. Commands and patches run with your full user
permissions.

## Install

bettercodex supports macOS 12 or newer and Linux with glibc 2.31 or newer.

```sh
curl -fsSL https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.sh | sh
```

Native releases are still in progress. Until they are ready, the installer
builds from source and needs rustup, a native C toolchain, and several gigabytes
of free space.

Open a new terminal, then run:

```sh
bcodex login
bcodex
```

## Clean up slop

```text
/review src/tui
```

Use `/review <target>` in the TUI or put `$review <target>` in any prompt. Review
edits the code. Point it at a messy part of the codebase and let it clean it up.

More detail is in the [install docs](docs/install.md) and
[slash command docs](docs/slash_commands.md).
