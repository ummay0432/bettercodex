# bettercodex

You cannot motivate agents to be proactive by tweaking the system prompt. You
have to do it through orchestration.

Claude Code and Codex steer the model toward the smallest change that gets the
task done. Over thousands of sessions, that compounds slop and technical debt.

bettercodex is a work in progress port of
[OpenAI Codex](https://github.com/openai/codex) trying to fix this in the
harness. The plan is to use hooks, Andrej Karpathy's
[`autoresearch`](https://github.com/karpathy/autoresearch), and context efficient
loops to make the agent notice problems, do the work, and clean up after itself.

Right now, bettercodex has an `autoresearch` inspired quality loop. One session
builds an evaluator. Fresh sessions then take turns improving the same task. The
harness keeps better results and throws away regressions.

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

## Quality loop

```text
/loop improve startup time
```

You can also put `$loop` anywhere in a prompt. The default is one evaluator
session and three fresh work sessions.

More detail is in the [install docs](docs/install.md) and
[slash command docs](docs/slash_commands.md).
