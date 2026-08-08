# bettercodex

bettercodex is a focused, private port of the OpenAI Codex CLI for a small
group of trusted operators.

## Install from source

Repository access is the invite gate. After accepting an invitation, sign in
with the [GitHub CLI](https://cli.github.com/), install Rust through rustup and
a native C toolchain, then clone the single Cargo package into a stable local
path and install it:

```sh
bcodex_source="${XDG_DATA_HOME:-$HOME/.local/share}/bettercodex/source"
mkdir -p "$(dirname "$bcodex_source")" &&
gh repo clone ummay0432/bettercodex "$bcodex_source" &&
cargo install --locked --path "$bcodex_source" --force --root "$HOME/.local"
```

Then open a new terminal, sign in with your own ChatGPT account, and launch
bettercodex from a project directory:

```sh
bcodex login
bcodex
```

To update the installed binary later, fast-forward that checkout and reinstall
the same package:

```sh
bcodex_source="${XDG_DATA_HOME:-$HOME/.local/share}/bettercodex/source"
git -C "$bcodex_source" pull --ff-only &&
cargo install --locked --path "$bcodex_source" --force --root "$HOME/.local"
```

[`INSTALL_COMMAND.txt`](INSTALL_COMMAND.txt) combines first install and later
updates into one copyable, idempotent command.

For a task that merits evaluator-backed iteration, use `/loop <task>` in the
TUI or include `$loop` anywhere in an interactive or non-interactive prompt.
The default runs one evaluator session followed by three fresh working
sessions; see [`docs/slash_commands.md`](docs/slash_commands.md#quality-loop)
for counts, progress, restoration, and repository-local evidence.

See [`docs/install.md`](docs/install.md) for supported platforms, migration from
the retired `bcodex update` command, and the full source-build workflow.
