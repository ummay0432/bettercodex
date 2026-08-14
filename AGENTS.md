# bettercodex

The reason we're making bettercodex is that, when you're working on a long-term
project that grows over time—for example, business operations—you rack up
compounding technical debt, trash, and scope creep through thousands of Codex
sessions because of the way the harness steers the model. It confines the model
to being extremely passive, the opposite of proactive, and steers it to make
the least amount of changes possible. This is where bettercodex differs: it lets
the model stretch its legs and live up to its potential. It gives the model the
freedom to be proactive.

Always write the project name as `bettercodex`, never `BetterCodex`.

Do not use emojis, except for Codex's established checkmark status marker.

The startup art is off limits: do not modify or delete it.

Do not create, edit, regenerate, or otherwise tweak model-facing context without
the user's explicit approval, including `AGENTS.md`, `prompts/system.md`,
`prompts/tool-catalogue.md`, other `prompts/*.md`, tool descriptions, and
model-visible errors.

Do not add audio or video support, dependencies, protocol items, runtime helpers,
tool descriptions, fixtures, or tests; bettercodex does not use either modality.

Keep Responses API reasoning summaries disabled; they add latency and output-token cost.

Rust builds and tests must clean up task-owned temporary files, fixtures,
caches, and isolated compiled artifacts, including on failure. Never remove the
checkout's shared `target/`, another session's artifact root, or a target still
referenced by a live Cargo or rustc process.

bettercodex is a focused port of [OpenAI Codex](https://github.com/openai/codex), not an independent implementation. For every retained behavior Codex already implements, inspect and port the current upstream source before editing; unexplained reimplementation or drift is forbidden unless [`product-direction.md`](docs/product-direction.md) explicitly requires the departure.

When comparing bettercodex with upstream Codex, temporarily clone the upstream repository into the workspace, compare against that local checkout instead of making repeated remote Git calls, and remove the clone afterward.

Do not invent Cargo, build, packaging, release, or test infrastructure; mirror current upstream Codex for retained workflows unless an explicit bettercodex requirement makes a deviation necessary.

Published bettercodex builds are immutable snapshots of a full public `main` revision encoded in the release tag. Update freshness follows the semantic version of the latest published full release; the encoded revision pins its exact source and installer.

Only the user decides when a release is ready; when explicitly asked to prepare or publish one, follow [`docs/releasing.md`](docs/releasing.md) for binary renewal and final publication.

Never create, edit, or finalize patch notes in `CHANGELOG.md` without the
user's explicit approval of their exact wording. Before changing
`CHANGELOG.md`, propose the patch notes to the user and wait for approval; once
approved, preserve that wording unless the user approves a revision.

## Test and validation discipline

- Before adding a checked-in test, evaluation, fixture, snapshot, or validation
  script, inspect the nearest existing bettercodex coverage and, for retained
  behavior, current upstream Codex coverage. Extend existing coverage only when
  it proves changed observable behavior that is not already covered; a code
  change does not by itself require a new test artifact.
- Do not test static or copied values, implementation details, or merely assert
  that removed logic remains absent. Reuse existing test helpers, and avoid
  adding production APIs, production functions, or parallel test infrastructure
  solely for tests.
- Keep one-off diagnostics and behavioral-evaluation inputs and results in
  task-owned temporary files and remove them after use. Check them in only when
  an explicit ongoing bettercodex requirement makes them a maintained gate.

## Start here

This file is the universal context layer. Before working, open only the matching
task-specific context; do not preload the whole folder:

- [`docs/product-direction.md`](docs/product-direction.md): before changing product scope, fixed model/runtime choices, supported platforms, or major dependencies.
- [`docs/development.md`](docs/development.md): before changing Rust, build scripts, tests, performance, or installation behavior.
- [`docs/inference.md`](docs/inference.md): before changing requests, Responses items, history, reasoning, compaction, caching, streaming, recovery, tools, or saved sessions.
- [`docs/instruction-hierarchy.md`](docs/instruction-hierarchy.md): before changing message roles, trust boundaries, repository/skill/tool context authority, prompt-injection defenses, or related evaluations.
- [`docs/terminal-ui.md`](docs/terminal-ui.md): before changing `src/tui/`, terminal lifecycle, rendering, composer behavior, or shortcuts.
- [`docs/model-facing-context.md`](docs/model-facing-context.md): before changing `AGENTS.md`, `prompts/*.md`, tool descriptions, or model-visible errors.

## Repository map

- `src/main.rs`, `src/input.rs`, and `src/auth.rs`: CLI entry, user input, and ChatGPT authentication.
- `src/agent.rs`, `src/context.rs`, `src/compaction.rs`, and `src/rollout.rs`: turns, context, compaction, saved JSONL sessions, and resume.
- `src/api.rs`, `src/api_sse.rs`, and `src/api_websocket.rs`: Responses requests, transports, streaming, and retries; `src/web_search.rs` projects hosted search items and citations.
- `src/tools.rs` and `src/process_runtime.rs`: the fixed function catalogue, file operations, and non-PTY Bash execution.
- `src/skills.rs`, `src/system_skills.rs`, `src/skill_settings.rs`, and `bundled-skills/`: local and embedded skills plus progressive disclosure.
- `src/tui/`: the Ratatui chat interface and terminal lifecycle.
- `prompts/`: exact model-facing system and tool context.

## Direct references

These links describe upstream OpenAI products or Rust, not bettercodex behavior. If
OpenAI documents the behavior you are working on, fetch the documentation
before designing or changing it. Never rely on memory or make assumptions where
an authoritative source exists. Apply the same rule to the official Rust sources below.

- CLI interaction: [interactive CLI](https://developers.openai.com/codex/cli/features#running-in-interactive-mode) and [slash commands](https://developers.openai.com/codex/cli/slash-commands).
- Agent context: [AGENTS.md](https://developers.openai.com/codex/guides/agents-md) and [skills](https://developers.openai.com/codex/skills).
- Authentication and permissions: [authentication](https://developers.openai.com/codex/auth) and [security, sandboxing, and approvals](https://developers.openai.com/codex/security).
- Automation: [non-interactive mode](https://developers.openai.com/codex/noninteractive) and [execution-policy rules](https://developers.openai.com/codex/exec-policy).
- Configuration comparison: [basics](https://developers.openai.com/codex/config-basic), [advanced configuration](https://developers.openai.com/codex/config-advanced), [reference](https://developers.openai.com/codex/config-reference), and [sample](https://developers.openai.com/codex/config-sample). bettercodex deliberately has no configuration framework.
- Upstream implementation: [Codex source](https://github.com/openai/codex) and [Codex build instructions](https://github.com/openai/codex/blob/main/docs/install.md).
- Rust language and tooling: [Book](https://doc.rust-lang.org/stable/book/), [standard library](https://doc.rust-lang.org/stable/std/), [Reference](https://doc.rust-lang.org/stable/reference/), [Cargo](https://doc.rust-lang.org/stable/cargo/), and [Clippy](https://doc.rust-lang.org/stable/clippy/).
- Rust design and performance: [API Guidelines](https://rust-lang.github.io/api-guidelines/), [rustdoc](https://doc.rust-lang.org/stable/rustdoc/), [Performance Book](https://nnethercote.github.io/perf-book/introduction.html), and [Nomicon](https://doc.rust-lang.org/stable/nomicon/).
