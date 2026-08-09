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

bettercodex supports Apple silicon macOS 12+, Ubuntu 22.04+ x86-64, and Debian
12+ x86-64. Native Windows 11 x64 is available as a developer preview while the
native automated and interactive terminal matrices are completed.

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

The installer downloads the matching binary from the latest published full
release, verifies its embedded version and exact source revision, and replaces
the command only after validation succeeds. Neither installs nor `bcodex update`
require Rust, Cargo, or native build tools. See the complete [installation
guide](docs/install.md).
