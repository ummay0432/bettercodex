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
- tool routing: ChatGPT-authenticated normal Responses with native parallel
  tool calls, exactly four ordinary function tools (`bash`, `read`, `write`, and
  `edit`), and hosted `web_search` with live text and image results; `read`
  handles both bounded UTF-8 text and local image attachments.

bettercodex does not expose client-side Code Mode, hosted Programmatic Tool
Calling, dynamic tool search, or a fallback tool route. Hosted web search is a
fixed Responses capability, and its URL citations are shown as visible,
clickable terminal links.

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

Live tool activity and output stay in transcript entries above the single-row
task status. Persistent processes belong in the operator's `tmux` session, not
a model-visible background-process protocol.

Do not add another provider, binary, app server, SDK, MCP layer, plugin system,
configuration framework, build system, or plugin hook unless the user gives a
concrete bettercodex use for it. Supported release targets are Apple
silicon macOS and x86-64 Ubuntu and Debian. Current upstream Codex source
remains authoritative for retained compatibility behavior on those targets.
