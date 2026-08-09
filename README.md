# bettercodex

Claude Code and Codex steer the model toward the smallest change that gets the
task done. Over thousands of sessions, that compounds slop and technical debt.

bettercodex is a focused, work-in-progress port of
[OpenAI Codex](https://github.com/openai/codex). It gives the agent room to own
engineering work proactively, including active review and cleanup, instead of
making the smallest possible patch the objective.

This is early, unofficial, and not an OpenAI product.

bettercodex has no Codex sandbox. Commands and patches run with your full user
permissions.

## Install

bettercodex supports macOS 12 or newer and Linux with glibc 2.31 or newer.
Native Windows 11 x64 is available as a developer preview while the native
automated and interactive terminal matrices are completed.

On macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.sh | sh
```

In Windows PowerShell 5.1 or newer:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm 'https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.ps1' | iex"
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
compile only what changed. On Windows, a commit that does not alter release
inputs is restamped from a verified cached or installed executable without
starting Rust or MSVC. A release-input content hash keeps that reuse exact even
when archive timestamps collide. Package versions are display metadata and do
not control updates. Missing Rust and Linux build tools are installed
automatically; on a new Mac, complete the Command Line Tools dialog the
installer opens and rerun the command once. Native Windows requires Visual
Studio 2022 C++ Build Tools and a Windows SDK; see the complete
[installation guide](docs/install.md) before the first build.
