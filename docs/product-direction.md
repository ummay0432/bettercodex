# Product direction and scope

Read this before changing what bettercodex is, what it supports, or which
upstream Codex behavior belongs here.

[`AGENTS.md`](../AGENTS.md) defines why bettercodex exists. This file defines
which Codex behavior belongs here and the fixed product boundaries.

## Relationship to Codex

We are not smarter or more capable than OpenAI's Codex team, and we are not
going to invent worse versions of things they have already solved. Codex is open
source. For Responses requests, reasoning continuity, compaction, prompt
caching, streaming, tool execution, and recovery, inspect the working Codex
implementation and take what is good.

The goal is not to copy all of Codex. Leave behind its app server, SDKs, support
for other models and providers, plugin system, MCP layer, configuration
framework, Node workspace, and Bazel build.

## Focused choices

- default model: `gpt-5.6-sol`;
- default reasoning effort: `xhigh`;
- available models: exactly `gpt-5.6-sol`, `gpt-5.6-terra`, and
  `gpt-5.6-luna`, exposed through `/model` with their GPT-5.6 reasoning
  efforts capped at `max`; bettercodex does not fetch or retain a broader model
  catalog;
- context and automatic-compaction limits: the GPT-5.6 Codex metadata shared by
  all three models: 272,000 raw, 258,400 effective, and 244,800 at automatic
  compaction;
- maximum output tokens: 128,000; and
- tool routing: the GPT-5.6 Responses Lite `code_mode_only` route. Responses
  exposes only `exec`/`wait`; retained tool implementations are available only
  as nested Code Mode tools.

Codex Code Mode is the upstream client-side V8 `exec`/`wait` path, not the
Responses API's hosted `programmatic_tool_calling` tool. Do not translate
between them or add hosted-PTC request fields unless current upstream Codex does.

`code_mode_only` is a transport route, not a reason to turn every tool stage
into a JavaScript workflow. Apply
[OpenAI's Programmatic Tool Calling selection boundary](https://developers.openai.com/api/docs/guides/tools-programmatic-tool-calling#choose-when-to-use-programmatic-tool-calling)
to the shape of each Code Mode cell:

- when one call is sufficient, the result shape is not reliably documented,
  fresh model judgment should follow a result, or the stage is adaptive,
  write/approval-sensitive, citation-heavy, or carries a native artifact, make
  one nested call, preserve its complete result, and return control to the
  model; and
- compose or batch nested calls only for a bounded, predictable, read-only stage
  where code can filter, join, rank, deduplicate, aggregate, or validate
  structured intermediate results into a materially smaller result. Constrain
  the eligible calls and make the result fields, call and retry limits, failure
  behavior, stopping condition, and handoff back to model judgment explicit;
  parallelize only independent side-effect-free reads.

`/model` is the only model-selection surface. It persists the selection as
focused bettercodex state without introducing providers, profiles, or a general
configuration framework. `/fast` is the only service-tier surface and persists
the last on/off choice for new sessions.

The runtime is one Cargo package and one `bcodex` binary. It contains the
inference loop and terminal UI for one operator. Commands and patches run with
the invoking user's permissions; bettercodex does not sandbox them.

The public installation channel is the latest user-authorized full release.
Each release tag combines the Cargo package version with the exact public
`main` revision, and installers download the matching prebuilt target binary.
Source compilation is a developer workflow, not an installation or update
path.

Live tool detail and the active background-terminal summary occupy dedicated
rows between the task status and the composer. bettercodex never folds either
surface into the busy status line.

Do not add another provider, binary, app server, SDK, MCP layer, plugin system,
configuration framework, build system, or plugin hook unless the user gives a
concrete bettercodex use for it. Supported release targets are Apple
silicon macOS, x86-64 Ubuntu and Debian, and native x86-64 Windows 11. Windows
support must remain target-gated: it must not add Windows-only instructions,
tools, or platform prose to the model-facing context on other platforms; Unix
shell prose must likewise remain absent from native Windows context. Current
upstream Codex source remains authoritative for retained compatibility behavior,
and
[`development.md`](development.md#native-windows-qualification) records the
native verification gate.
