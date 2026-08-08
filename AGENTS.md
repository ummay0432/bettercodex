# bettercodex

The reason we're making bettercodex is that, when you're working on a long-term
project that grows over time—for example, business operations—you rack up
compounding technical debt, trash, and scope creep through thousands of Codex
sessions because of the way the harness steers the model. It confines the model
to being extremely passive, the opposite of proactive, and steers it to make
the least amount of changes possible. This is where bettercodex differs: it lets
the model stretch its legs and live up to its potential. It gives the model the
freedom to be proactive.

Do not use emojis, except for Codex's established checkmark status marker.

Do not leave Rust build trash behind: after integrating Rust work, remove its linked worktree and clean its now-inactive Cargo target using the workflow in [`development.md`](progressive_disclosure/development.md).

bettercodex is a focused port of [OpenAI Codex](https://github.com/openai/codex), not an independent implementation. For every retained behavior Codex already implements, inspect and port the current upstream source before editing; unexplained reimplementation or drift is forbidden unless [`product-direction.md`](progressive_disclosure/product-direction.md) explicitly requires the departure.

Do not invent Cargo, build, packaging, release, or test infrastructure; mirror current upstream Codex for retained workflows unless an explicit BetterCodex requirement makes a deviation necessary.

## Start here

This file is the universal context layer. Before working, open only the matching
task-specific context; do not preload the whole folder:

- [`progressive_disclosure/product-direction.md`](progressive_disclosure/product-direction.md): before changing product scope, fixed model/runtime choices, supported platforms, or major dependencies.
- [`progressive_disclosure/development.md`](progressive_disclosure/development.md): before changing Rust, build scripts, tests, performance, or installation behavior.
- [`progressive_disclosure/inference.md`](progressive_disclosure/inference.md): before changing requests, Responses items, history, reasoning, compaction, caching, streaming, recovery, tools, or saved sessions.
- [`docs/instruction-hierarchy.md`](docs/instruction-hierarchy.md): before changing message roles, trust boundaries, repository/skill/tool context authority, prompt-injection defenses, or related evaluations.
- [`progressive_disclosure/terminal-ui.md`](progressive_disclosure/terminal-ui.md): before changing `src/tui/`, terminal lifecycle, rendering, composer behavior, or shortcuts.
- [`progressive_disclosure/model-facing-context.md`](progressive_disclosure/model-facing-context.md): before changing `AGENTS.md`, `prompts/*.md`, tool descriptions, or model-visible errors.

## Repository map

- `src/main.rs`, `src/input.rs`, and `src/auth.rs`: CLI entry, user input, and ChatGPT authentication.
- `src/agent.rs`, `src/context.rs`, `src/compaction.rs`, and `src/rollout.rs`: turns, context, compaction, saved JSONL sessions, and resume.
- `src/api.rs`, `src/api_sse.rs`, `src/api_websocket.rs`, and `src/web_search.rs`: Responses requests, transports, streaming, retries, and web search.
- `src/tools/`: the JavaScript exec runtime, nested tool catalogue, command execution, and patch application.
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
