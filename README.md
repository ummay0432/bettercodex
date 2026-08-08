# bettercodex

bettercodex is a focused, private port of the OpenAI Codex CLI for a small
group of trusted operators.

## Install from source

Repository access is the invite gate. After accepting an invitation, sign in
with the [GitHub CLI](https://cli.github.com/), then install `curl`, Rust through
rustup, and a native C toolchain. Copy and run the one-line
[`INSTALL_COMMAND.txt`](INSTALL_COMMAND.txt). It downloads one immutable source
snapshot, builds entirely in a temporary directory, and automatically removes
the source, Cargo dependency cache, V8 download, and compilation artifacts
afterward.

Then open a new terminal, sign in with your own ChatGPT account, and launch
bettercodex from a project directory:

```sh
bcodex login
bcodex
```

Installed builds check their embedded source revision against private `main`
after the TUI renders. When an update is available, run `bcodex update` in
another terminal. It uses the same temporary source-build flow as
[`INSTALL_COMMAND.txt`](INSTALL_COMMAND.txt), so each update builds from scratch
and retains no BetterCodex source checkout or multi-gigabyte build cache. The
installed binary, a small shell-profile PATH entry when needed, and the required
Rust and native toolchains remain.

For a task that merits evaluator-backed iteration, use `/loop <task>` in the
TUI or include `$loop` anywhere in an interactive or non-interactive prompt.
The default runs one evaluator session followed by three fresh working
sessions; see [`docs/slash_commands.md`](docs/slash_commands.md#quality-loop)
for counts, progress, restoration, and repository-local evidence.

For an active engineering review and evidence-backed refactoring, use
`/review <target>` in the TUI or invoke `$review <target>` in any prompt. The
workflow develops a deep understanding of the target, applies explicit quality
criteria, and acts on clear net improvements; see
[`docs/slash_commands.md`](docs/slash_commands.md#engineering-review).

See [`docs/install.md`](docs/install.md) for supported platforms, temporary disk
requirements, migration from older installers, and the full source-build
workflow.
