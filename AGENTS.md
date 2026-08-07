# bettercodex

bettercodex is my personal Codex CLI fork. I have been an avid Codex user since
forever, but Codex has grown into a product made for everyone, full of
compromises, bloat, and trash I do not need. bettercodex strips it back to a
bare coding-agent harness bespoke to how I work: one Cargo package, one
`bcodex` binary, `gpt-5.6-sol`, the inference loop, local tools, and a terminal
UI for one operator. Commands and patches run with the invoking user's
permissions; bettercodex does not sandbox them. Its two non-negotiable success
criteria are that tool usage must be the same as Codex CLI, if not better, and
that there must be absolutely zero model degradation compared with Codex CLI.
Generic coding-agent instructions chain the model down: do the least amount
possible, ignore problems outside the exact task, prefer the smallest patch,
and stop as soon as a local test passes. bettercodex should let the model
stretch its legs and be genuinely proactive without losing any of Codex's
capability. I am a CEO who knows my way around terminals and Linux but does
**zero** programming; I make the product decisions, and the agent owns the
engineering. My business codebases were built through thousands of AI sessions,
many when the models were stupid—very stupid—so they contain slop: duplicated
paths, shallow fixes, needless abstractions, dead code, disorganization, and
implementations far from the best design today. Keep them tidy; when relevant
work exposes poor engineering, refactor, delete, consolidate, or clean it up
instead of compounding it. Do not invent unrelated features, but do not preserve
bad engineering just to keep a diff small.

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
- `prompts/`: exact model-facing system and tool context; `prompts/tool-context.md` is the reproducible context audit.

## Direct references

These links describe upstream OpenAI products or Rust, not bettercodex behavior. If
OpenAI documents the behavior you are working on, fetch the documentation
before designing or changing it. Never rely on memory or make assumptions where
an authoritative source exists. Apply the same rule to the official Rust sources below.

- GPT-5.6 prompting: [model guidance and prompting best practices](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-5.6#prompting-best-practices).
- CLI interaction: [interactive CLI](https://developers.openai.com/codex/cli/features#running-in-interactive-mode) and [slash commands](https://developers.openai.com/codex/cli/slash-commands).
- Agent context: [AGENTS.md](https://developers.openai.com/codex/guides/agents-md) and [skills](https://developers.openai.com/codex/skills).
- Authentication and permissions: [authentication](https://developers.openai.com/codex/auth) and [security, sandboxing, and approvals](https://developers.openai.com/codex/security).
- Automation: [non-interactive mode](https://developers.openai.com/codex/noninteractive) and [execution-policy rules](https://developers.openai.com/codex/exec-policy).
- Configuration comparison: [basics](https://developers.openai.com/codex/config-basic), [advanced configuration](https://developers.openai.com/codex/config-advanced), [reference](https://developers.openai.com/codex/config-reference), and [sample](https://developers.openai.com/codex/config-sample). bettercodex deliberately has no configuration framework.
- Upstream implementation: [Codex source](https://github.com/openai/codex) and [Codex build instructions](https://github.com/openai/codex/blob/main/docs/install.md).
- Rust language and tooling: [Book](https://doc.rust-lang.org/stable/book/), [standard library](https://doc.rust-lang.org/stable/std/), [Reference](https://doc.rust-lang.org/stable/reference/), [Cargo](https://doc.rust-lang.org/stable/cargo/), and [Clippy](https://doc.rust-lang.org/stable/clippy/).
- Rust design and performance: [API Guidelines](https://rust-lang.github.io/api-guidelines/), [rustdoc](https://doc.rust-lang.org/stable/rustdoc/), [Performance Book](https://nnethercote.github.io/perf-book/introduction.html), and [Nomicon](https://doc.rust-lang.org/stable/nomicon/).
