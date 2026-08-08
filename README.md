# bettercodex

bettercodex is a focused, private port of the OpenAI Codex CLI for a small
group of trusted operators.

## Install

Repository access is the invite gate. After accepting an invitation and
signing in with the [GitHub CLI](https://cli.github.com/), install Python 3,
Rust through rustup, and a native C toolchain. Then run:

```sh
gh api -H 'Accept: application/vnd.github.raw+json' repos/ummay0432/bettercodex/contents/scripts/install.sh | sh
```

Then open a new terminal, sign in with your own ChatGPT account, and launch
bettercodex from a project directory:

```sh
bcodex login
bcodex
```

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

The installer downloads the newest stable source tag and compiles it for the
current Mac or Linux machine. Interactive sessions check those private tags in
the background after the TUI is ready. When an update is available, build and
install it from another terminal:

```sh
bcodex update
```

The freshly compiled runtime and embedded resources are checked before the old
binary is replaced. Build dependencies and a persistent Cargo cache are reused
on later updates.

See [`docs/install.md`](docs/install.md) for first-time GitHub setup, supported
platforms, update behavior, privacy boundaries, and release instructions.
