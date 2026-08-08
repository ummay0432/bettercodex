# bettercodex

bettercodex is a focused, private port of the OpenAI Codex CLI for a small
group of trusted operators.

## Install from source

Repository access is the invite gate. After accepting an invitation, sign in
with the [GitHub CLI](https://cli.github.com/), then install `curl`, Rust through
rustup, and a native C toolchain. Copy and run the one-line
[`INSTALL_COMMAND.txt`](INSTALL_COMMAND.txt). It downloads one immutable source
commit, verifies the resulting binary and embedded resources, rechecks private
`main`, and atomically installs only a matching build. It automatically removes
the source and compilation artifacts after success, failure, or interruption,
while retaining Cargo dependency downloads and verified V8 artifacts for the
next update.

Then open a new terminal, sign in with your own ChatGPT account, and launch
bettercodex from a project directory:

```sh
bcodex login
bcodex
```

Installed builds check their embedded source revision against private `main`
after the TUI renders. When an update is available, run `bcodex update` in
another terminal. It uses the same temporary source-build flow as
[`INSTALL_COMMAND.txt`](INSTALL_COMMAND.txt), so each update gets a fresh source
tree and compilation target without redownloading unchanged Cargo dependencies
or V8 artifacts. It retains no BetterCodex source checkout or multi-gigabyte
build cache.

Only commits pushed to the private repository's `main` branch are distributable;
uncommitted, unpushed, and other-branch development work is not visible to
another device. Missing or edited embedded system files are integrity-checked
and atomically repaired when the updated binary launches.

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

See [`docs/install.md`](docs/install.md) for supported platforms, temporary disk
requirements, migration from older installers, and the full source-build
workflow.
