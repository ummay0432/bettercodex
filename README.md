# bettercodex

bettercodex is a focused public port of the OpenAI Codex CLI for people who
want a more proactive coding agent.

## Install

Copy and run the one-line [`INSTALL_COMMAND.txt`](INSTALL_COMMAND.txt). It
resolves the latest complete BetterCodex release, downloads the roughly 22 MB
compressed executable for this Mac or Linux computer, verifies its checksum,
version, source revision, V8 runtime, and embedded resources, then atomically
installs it. The normal path needs `curl` but no npm, GitHub login, Rust,
compiler, or local source build.

Then open a new terminal, sign in with your own ChatGPT account, and launch
bettercodex from a project directory:

```sh
bcodex login
bcodex
```

Installed builds compare their embedded source revision with the latest
published release after the TUI renders. When an update is available, run
`bcodex update` in another terminal. It streams a compact patch from the
installed revision when available, otherwise a roughly 18 MB full native
update, and atomically installs the verified executable. It compiles zero Cargo
packages, never enters source fallback, and retains no Rust build cache. A
bounded source build exists only in the bootstrap installer when no compatible
first-install asset exists.

Only complete, non-prerelease GitHub Releases are distributed. Public branches
and tags remain visible but do not trigger user updates. Missing or edited
embedded system files are integrity-checked and atomically repaired when the
updated binary launches.

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

See [`docs/install.md`](docs/install.md) for supported platforms, verification,
fallback, and migration details. [`spec-install.md`](spec-install.md) defines
the release and updater contract.
