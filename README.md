# bettercodex

You cannot motivate agents to be proactive by tweaking the system prompt. You
have to do it through orchestration.

Claude Code and Codex steer the model toward the smallest change that gets the
task done. Over thousands of sessions, that compounds slop and technical debt.

bettercodex is a work in progress port of
[OpenAI Codex](https://github.com/openai/codex) trying to fix this in the
harness. The plan is to use hooks and active engineering review to make the
agent notice problems, do the work, and clean up after itself.

$review is goated, use rigirously to deslop for the time being

This is early, unofficial, and not an OpenAI product.

bettercodex has no Codex sandbox. Commands and patches run with your full user
permissions.

## Install

bettercodex supports macOS 12 or newer and Linux with glibc 2.31 or newer.

```sh
curl -fsSL https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.sh | sh
```

Open a new terminal, then run:

```sh
bcodex login
bcodex
```

The installer pins the current public `main` commit, builds it locally, verifies
the resulting binary, and installs it atomically. Later, `bcodex update` repeats
that flow only when the exact public `main` revision changes. Cargo downloads,
compiled dependencies, and incremental bettercodex state stay warm, so updates
compile only what changed. A release-input content hash keeps that reuse exact
even when archive timestamps collide. Package versions are display metadata and
do not control updates. Missing Rust and Linux build tools are installed
automatically; on a new Mac, complete the Command Line Tools dialog the
installer opens and rerun the command once.
