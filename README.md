# BetterCodex

A bare coding-agent harness specialized for `gpt-5.6-sol`.

It contains the inference loop and its terminal interface: ChatGPT Codex
authentication, Responses streaming, incremental in-memory context, native
compaction, local tools, and a Codex-style Ratatui chat surface. There is no app
server, plugin system, MCP layer, configuration framework, Node workspace, or
Bazel build.

## Install

Use an existing Codex ChatGPT login at
`${CODEX_HOME:-$HOME/.codex}/auth.json`, then install the single binary:

```sh
cargo install --locked --path . --force --root "$HOME/.local"
bcodex
```

For ephemeral credentials, set `CODEX_ACCESS_TOKEN` and optionally
`CHATGPT_ACCOUNT_ID` instead.

Pass a prompt for a one-shot turn:

```sh
bcodex "inspect this repository and report its architecture"
```

Attach one or more local images, or continue the most recent session for the
current repository:

```sh
bcodex --image screenshot.png --image-detail original "find the visual bug"
bcodex resume --last "continue where we stopped"
bcodex resume 6f47d9be-8fca-4a4d-90af-3d03b72ce487
```

Sessions are private append-only JSONL journals under
`${CODEX_HOME:-$HOME/.codex}/bettercodex/sessions`. They retain API output
items in order, exact tool-call IDs and outputs, token/cache usage, compaction
state, and unfinished-turn recovery. `resume --last` is scoped to the current
canonical working directory; an explicit ID resumes its saved working
directory. An active journal has one process owner, so a concurrent resume
fails instead of interleaving two inference histories.

Inside the TUI, `/resume` opens a searchable picker scoped to the current
working directory. `Tab` toggles all BetterCodex sessions, `Up` and `Down`
browse, and `Enter` switches sessions without risking the current session if
loading the target fails. `/resume SESSION_ID` switches directly. Resume is
disabled while an agent turn is active.

With no arguments, `bcodex` opens the interactive TUI. Assistant text and
reasoning stream into the transcript, tool calls update in place, and the
footer shows the fixed model, Git repository, branch, and effective context
usage. Finalized cells stay in the terminal's normal scrollback; BetterCodex
does not take over the alternate screen. Shell and patch tools run with the
invoking user's permissions.

Prompt recall uses Codex's `${CODEX_HOME:-$HOME/.codex}/history.jsonl`, so
existing Codex prompts and resumed BetterCodex prompts are available in the
composer. The core shortcuts are:

- `Enter` submits, or queues steering for the next model/tool boundary of an
  active turn. Pending steering stays visible above the composer; `Esc`
  interrupts and sends it immediately.
- `Tab` submits when idle and queues a FIFO follow-up while a turn is active.
  `Alt+Up` or `Shift+Left` pulls the latest queued follow-up back into the
  composer. Active file and slash completion still take precedence over these
  bindings.
- `Shift+Enter` or `Ctrl+J` inserts a newline.
- `Esc` interrupts an active turn; `Ctrl+C` exits.
- `Up` and `Down` move through older and newer prompt history.
- `Option+Left` and `Option+Right` jump by word, and `Option+Backspace`
  deletes the previous word on macOS (`Alt` elsewhere). `Ctrl+W` also deletes
  the previous word.
- The mouse wheel and the terminal's normal scrollback controls browse the
  finalized transcript.
- `?` on an empty composer opens the shortcut reference.
- Typing `/` opens completion for `/clear`, `/context`, `/compact`, `/resume`,
  `/help`, `/tools`, and one combined `/quit`, `/exit` entry; type `/q` and press
  `Enter` to quit quickly. `/compact` manually compacts the current conversation
  and can be interrupted with `Esc`. `/context` opens a colored breakdown of the
  effective window, current request categories, free space before
  auto-compaction, and reserved headroom.
- Typing `@` opens Git-ignore-aware fuzzy file search. `Up` and `Down` select a
  result, `Enter` or `Tab` inserts its repository-relative path, and `Esc`
  closes the search.
- Typing `$` opens fuzzy completion for skills discovered under `.bcodex/skills`
  from the repository root through the working directory and under
  `${BCODEX_HOME:-$HOME/.bcodex}/skills`. `Enter` or `Tab` binds the selected
  skill to that prompt; queued follow-ups and steering preserve the binding.

When stdin or stdout is redirected, no-argument invocation falls back to the
plain line interface. Passing a prompt remains a one-shot invocation.

The Responses client prefers WebSockets and reuses completed responses with a
guarded incremental input delta. It reconnects with full history when that
connection-local state is gone and falls back to HTTP SSE when WebSocket
upgrade is unavailable. Stable request content uses an explicit prompt-cache
breakpoint; cache reads and writes are recorded from backend usage rather than
inferred.

## Tools

BetterCodex always exposes one JavaScript tool runtime for Sol; there is no tool
mode or selector. The Responses request exposes the freeform `exec` tool and its
`wait` continuation tool. `exec` runs JavaScript in an embedded V8 isolate and
gives that program the fixed nested catalogue: `apply_patch`, `exec_command`,
`log_papercut`, `update_plan`, `view_image`, and `write_stdin`. This
unconditionally ports the efficient orchestration path that upstream Codex
calls `code_mode_only`, while command sessions persist across calls and run
with the invoking user's permissions. `log_papercut` appends bounded friction
notes to `PAPERCUTS.md` at the Git worktree root.

Sol therefore gets programmatic composition, parallel calls, and intermediate
result reduction by default. On the ChatGPT Responses Lite route this is a
client-owned `exec` contract, not the public API's hosted `program` and
`program_output` item protocol.

Read the exact model-visible catalogue in
[`prompts/tool-catalogue.md`](prompts/tool-catalogue.md), or print the generated text
from the binary:

```sh
bcodex --tool-catalogue
```

[`prompts/tool-context.md`](prompts/tool-context.md) records the complete
tool-related request prefix, the dynamic world-state messages beside it, and
reproducible per-tool token-cost estimates.

## Validate

```sh
cargo fmt --all --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Licensed under [Apache-2.0](LICENSE).
