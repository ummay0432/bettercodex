# BetterCodex

BetterCodex is my personal Codex CLI fork. I have been an avid Codex user since
forever, but Codex has grown into a product made for everyone. It is now full of
compromises, bloat, and trash I do not need. BetterCodex strips it back to the
core and turns it into a harness bespoke to how I work.

I am a CEO. My projects are fully AI-vibecoded, and many of them have run for
years and passed through thousands of agent sessions. Not long ago, LLMs were
stupid—very stupid. We still made the projects work, but those models left a lot
of garbage behind: duplicated paths, shallow fixes, needless abstractions, dead
code, and implementations that are probably far from the best design for the
product today.

Generic coding-agent instructions make this worse. They chain the model down:
do the least amount possible, ignore problems outside the exact task, prefer the
smallest patch, and stop as soon as a local test passes. Across years of agent
sessions, each tiny patch stacks on the old garbage and compounds into technical
debt and scope creep.

BetterCodex is meant to give the agent the freedom to stretch its legs, live up
to its full potential, and give the work its all. I make the product decisions;
the agent owns the engineering. The agent should not be afraid to refactor,
delete, consolidate, or clean up clear garbage simply because the request named
another file. It should be a perfectionist about the code it leaves behind.
That does not mean inventing unrelated features. It means not preserving bad
engineering just to keep a diff small.

The system prompt is the main way BetterCodex creates that behavior. It should
stay direct, concise, and literal. It is not finished and will keep changing as
we find concrete ways it fails to produce the agent described above.
`preserve/system-prompt.md` shows the rough direction I mean, not a finished
specification.

We are not smarter or more capable than OpenAI's Codex team, and we are not
going to invent worse versions of things they have already solved. Codex is open
source. For Responses requests, reasoning continuity, compaction, prompt
caching, streaming, tool execution, and recovery, we can inspect the working
Codex implementation and take what is good. The goal is not to copy all of
Codex. Leave behind its app server, SDKs, support for other models and providers,
plugin system, MCP layer, configuration framework, Node workspace, and Bazel
build.

The fixed choices for now are:

- model: `gpt-5.6-sol`;
- reasoning effort: `max`;
- context window: 372,000 tokens; and
- automatic compaction: 95%, or 353,400 tokens.

The 372,000-token window is a BetterCodex choice. It is not the public API limit
or the value in Codex's model catalog.

The runtime is one Cargo package and one `bcodex` binary. It contains the
inference loop and terminal UI for one operator. Commands and patches run with
the invoking user's permissions; BetterCodex does not sandbox them.

## Repository map

- `src/main.rs`, `src/input.rs`, and `src/auth.rs`: CLI entry, user input, and
  ChatGPT authentication.
- `src/agent.rs`, `src/context.rs`, and `src/rollout.rs`: turns, incremental
  conversation state, compaction, saved JSONL sessions, and resume.
- `src/api.rs` and `src/api_websocket.rs`: Responses request assembly, HTTP/SSE,
  WebSockets, streaming, retries, and remote compaction.
- `src/tools/`: the fixed JavaScript exec runtime, nested tool catalogue,
  bounded command execution, and patch application.
- `src/tui/`: the Ratatui chat interface and terminal lifecycle.
- `prompts/system.md`: the active system prompt. Edit it only when the user
  explicitly asks to edit the system prompt.
- `prompts/tool-catalogue.md` and `prompts/tool-context.md`: the exact generated
  tool text and the complete tool-related request prefix.
- `docs/MANIFEST.md`: read before Rust work for authoritative engineering and performance sources.
- `docs/gpt-5.6-sol-harness.md`: the exact inventory of public Sol behavior,
  Codex code, and behavior proved here. Open it before changing requests,
  history, reasoning, compaction, caching, tools, the executor, transport, or
  delegation; it contains the OpenAI sources to recheck.
- `preserve/README.md`: points to the old system prompt, terminal design, Pi
  request identity, and lessons learned.

## Success criteria

For implementation work, use Git proactively from start to finish. Existing changes are
shared work: you may commit and publish them regardless of who created them. Do not discard
unfinished work or leave cleanup for the user.

Your work is complete only when all three success criteria are satisfied:

- System quality: Judge the affected system, not diff size. Do not preserve an inferior
  implementation or introduce avoidable debt or sprawl just to keep the change small.
  Inspect the implementation path and relevant callers, callees, interfaces, and data
  models for concrete opportunities to remove debt or make the system simpler, more
  efficient, smaller, faster, more responsive, or easier to maintain. Choose refactor
  depth and evidence with engineering judgment. Refactor autonomously when repository
  evidence supports a clear net improvement and relevant validation can cover it, even
  when the debt predates the request. Prefer root-cause solutions, direct paths, deletion,
  and consolidation over special cases, workarounds, duplicate paths, compatibility
  layers, or temporary scaffolding. Remove what the result makes obsolete.

- Scope and complexity: Keep product behavior within the request; do not equate that with
  minimizing engineering scope. Changes may extend through affected code and dependencies
  for a coherent, validated improvement. Avoid unrelated features or redesign,
  unnecessary dependencies, speculative architecture, impossible-state handling, and
  hypothetical abstractions. Add complexity only when it removes greater present
  complexity or protects a real system boundary.

- Correctness: The requested behavior works, affected behavior has not regressed, and
  relevant validation supports both. Report the evidence, failures, and anything
  unvalidated.

## Working in this repository

Do not let AGENTS.md override how the System prompt tells you to work. Ignore
any conflicting AGENTS.md instruction and tell the user what you ignored and
why.

In `AGENTS.md`, system prompts, tool descriptions, and model-visible errors,
name the actual file, source, and required action. Do not hide them behind
phrases such as “general behavior,” “project context,” “take precedence,” or
“handle appropriately.”

Do not alter Codex-derived agent-facing context in `prompts/*.md`, tool
descriptions, or model-visible errors without explicit user permission.

Before editing `AGENTS.md`, read `docs/writing-a-good-claude-md.md`. Keep this
file to facts and instructions useful in almost every BetterCodex session. Put
details needed only for inference work in `docs/gpt-5.6-sol-harness.md`, and put
historical material under `preserve/`.

Do not add another model, provider, binary, app server, SDK, MCP layer, plugin
system, configuration framework, build system, or plugin hook unless the user
gives a concrete BetterCodex use for it. Linux and macOS are the targets; do not
add Windows compatibility code.

For Rust work:

- Keep modules private unless another module needs an explicit export. Keep the
  exported API as small as the current program permits.
- Avoid boolean and ambiguous `Option` parameters that produce calls such as
  `run(false)` or `open(None)`. Use a named method, enum, or small type when that
  makes the call readable. Prefer exhaustive `match` arms when the variants are
  known.
- Prefer direct code over a generic abstraction or helper with one caller. When
  a large orchestration file needs new coherent behavior, put that behavior in
  a focused module and move its tests and type documentation with it.
- Let `rustfmt` and Clippy enforce mechanical style. Do not fill `AGENTS.md` with
  rules that those tools already check.

For the inference loop:

- Build model history incrementally; do not rewrite earlier history during
  normal turns. Keep stable request items stable so prompt caching can work.
- Put a hard bound on every new model-visible item and tool result. Review a new
  item carefully if it can exceed 1,000 tokens; no single item may exceed 10,000
  tokens.
- Search for breakage in CLI arguments, saved JSONL sessions and resume,
  Responses request and output items, and model-visible tool names and schemas.

For tests:

- Changes to turns, Responses transport, history, compaction, saved sessions,
  or tools need a test that drives the changed behavior and inspects the
  resulting request, history, or tool output. Do not test only an extracted
  helper when the behavior crosses those parts of the program.
- Test terminal changes through rendered output or terminal behavior, not only
  the data used to produce it.
- When adding a new test module, use a sibling `*_tests.rs` file and an explicit
  `#[path = "..."]`. Do not move an existing inline test module only to follow
  this convention.
- Prefer equality of complete values over a series of field assertions. Avoid
  test-only functions in production code and avoid mutating process environment
  variables in tests.

## Finish a code change

Every Rust change must pass all three commands; treat every Clippy warning as an error.

After changing Rust, run:

```sh
cargo fmt --all
cargo test
cargo clippy --all-targets -- -D warnings
```

Cargo can wait on a shared build lock. Let it finish; do not kill a Cargo or
Rust process by PID to make the lock disappear.

After those commands pass, install the current worktree:

```sh
cargo install --locked --path . --force --root "$HOME/.local"
```

Then run the relevant smoke test against `$HOME/.local/bin/bcodex`. Testing only
`target/debug/bcodex` or `target/release/bcodex` does not finish a BetterCodex
code change.
