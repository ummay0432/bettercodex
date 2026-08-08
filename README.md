# bettercodex

bettercodex is a focused, private port of the OpenAI Codex CLI for a small
group of trusted operators.

## Build from source

Repository access is the invite gate. After accepting an invitation, install
Rust through rustup and a native C toolchain, then clone and build the single
Cargo package:

```sh
gh repo clone ummay0432/bettercodex
cd bettercodex
cargo build --locked
cargo install --locked --path . --force --root "$HOME/.local"
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

See [`docs/install.md`](docs/install.md) for supported platforms and the full
source-build workflow.
