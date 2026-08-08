# bettercodex

bettercodex is a focused public port of the OpenAI Codex CLI for people who
want a more proactive coding agent.

## Install from source

Install `curl`, Rust through rustup, and a native C toolchain. Copy and run the
one-line
[`INSTALL_COMMAND.txt`](INSTALL_COMMAND.txt). It downloads one immutable source
commit, verifies the resulting binary and embedded resources, rechecks public
`main`, and atomically installs only a matching build. It automatically removes
the source, compiler scratch space, and BetterCodex-owned compilation output
after success, failure, or interruption. Cargo downloads, verified V8
artifacts, and compatible compiled dependencies remain cached for the next
update.

Then open a new terminal, sign in with your own ChatGPT account, and launch
bettercodex from a project directory:

```sh
bcodex login
bcodex
```

Installed builds check their embedded source revision against public `main`
after the TUI renders. When an update is available, run `bcodex update` in
another terminal. It uses the same temporary source-build flow as
[`INSTALL_COMMAND.txt`](INSTALL_COMMAND.txt), so each update gets a fresh source
tree while reusing the compiled dependency graph. A normal code-only update
therefore rebuilds BetterCodex itself instead of recompiling hundreds of
unchanged packages. The cache keeps one compatible generation per host and is
replaced when the toolchain, manifest, lockfile, or build wrapper changes. It
can use several gigabytes and can be deleted whenever disk space matters more
than update speed.

Only commits pushed to `main` are distributed by the installer and updater.
Because the repository is public, any branch or tag pushed to GitHub is visible
even when it is not distributed. Missing or edited embedded system files are
integrity-checked and atomically repaired when the updated binary launches.

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
