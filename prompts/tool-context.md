# Tool context sent to GPT-5.6 Sol

This file records the bettercodex tool-related input that enters the model's
context window. It is documentation and is not itself sent to the model.

The audit baseline is OpenAI Codex commit
`1669c2403f793d0230065397dfc25f52b844244e`, which bettercodex pins for the
shared Code Mode protocol and utility crates. The fixed catalogue, wrapper
specifications, and upstream description renderer were rechecked against Codex
commit
[`c5d94319715d2598a2e8b2b7d0e21a3b1e83aec6`](https://github.com/openai/codex/tree/c5d94319715d2598a2e8b2b7d0e21a3b1e83aec6)
on 2026-08-07. bettercodex keeps the runtime protocol and Codex's
schema-to-TypeScript conversion. Its fixed seven-tool surface uses one compact
declaration block instead of Codex's dynamic per-tool headings, prose, wrapper,
and fenced declaration.

## Request order

Every normal Responses request has two stable model-context channels:

1. The top-level `instructions` field contains `prompts/system.md`, with
   surrounding whitespace removed. Its `<system_instructions>` label identifies
   the bettercodex-owned behavioral contract. The Responses API's
   [instruction-following contract](https://developers.openai.com/api/docs/guides/text#message-roles-and-instruction-following)
   applies this as developer-level instruction.
2. The `input` array begins with an `additional_tools` developer item containing
   the typed top-level `exec` and `wait` specifications.

The remaining input is chronological conversation state. World state is loaded
at session start and reinserted if compaction removes it:

3. A developer `<environment_context>` message.
4. A user `<repository_context>` message containing the applicable
   `AGENTS.override.md` or `AGENTS.md` files.
5. When at least one enabled skill permits implicit invocation, a user
   `<available_skills>` message containing only the bounded name, description,
   and path catalogue.
6. Any explicitly selected full skill bodies, each in a user
   `<skill_context>` message.
7. The unchanged current user request.

Selected skill context is inserted immediately before its user request, so that
request is the final item when a turn is admitted. Subsequent assistant items,
tool calls, and tool outputs extend the dynamic trajectory in chronological
order. `src/api.rs` assembles the first two channels, `src/context.rs` assembles
world state, and `src/agent.rs` orders selected skill context and the request.

The `additional_tools` envelope is:

```json
{
  "type": "additional_tools",
  "role": "developer",
  "tools": ["the exec specification", "the wait specification"]
}
```

`instructions` and the typed `additional_tools` input item occupy distinct
request channels; JSON serialization order does not change their authority.
Under OpenAI's
[instruction hierarchy](https://model-spec.openai.com/2025-12-18.html#chain_of_command),
bettercodex cannot supply OpenAI's root or system layer. The top-level
`instructions` field is the closest API-controlled equivalent: a dedicated
developer-authority system-prompt channel above user-role repository, skill,
and conversation context.

## Token accounting

GPT-5.6 Sol's authoritative token count is the `usage.input_tokens` returned by
the backend. OpenAI does not publish a tokenizer or a rule for independently
tokenizing an `additional_tools` item. The figures below therefore provide two
reproducible estimates:

- `o200k` is the token count of the compact JSON or exact text using
  `o200k_base` from `tiktoken`.
- `bytes/4` is Codex's conservative `ceil(UTF-8 bytes / 4)` estimator, also
  used by bettercodex for text history estimates.

The JSON figures use compact serialization with sorted object keys. Counts are
the audited 2026-08-07 snapshot. `./scripts/dev.py tool-context --check`
renders the actual request items and verifies the stable rows;
`./scripts/dev.py tool-context --update` rewrites all three tables. The command
uses pinned `tiktoken` 0.11.0 through `uv` when the module is not already
installed. The request retains a session-scoped `prompt_cache_key` and relies
on the Responses API's default implicit prompt caching. It sends no explicit
cache breakpoint or `prompt_cache_options`. Caching reduces repeated input
billing; it does not remove cached input from the active context window.

The immediately preceding item was 11,370 bytes, 2,964 `o200k` tokens, and 2,843
bytes/4 tokens. The current item retains the same two request tools, seven nested
tools, input fields, enums, JavaScript names, and runtime behavior while reducing
those estimates by 55.4% of bytes, 53.5% of `o200k` tokens, and 55.4% of bytes/4
tokens. It also replaces three vague `unknown` returns with their implemented
shapes: empty objects for `apply_patch` and `update_plan`, and a string for
`web__run`. Relative to the original 20,440-byte catalogue, the reduction is
75.2%.

<!-- bcodex-tool-context:stable:start -->
| Injected component | UTF-8 bytes | o200k | bytes/4 |
| --- | ---: | ---: | ---: |
| Complete stable harness input: `instructions` plus `additional_tools` | 13,819 | 3,148 | 3,455 |
| Complete `additional_tools` developer item | 5,073 | 1,379 | 1,269 |
| Top-level `exec` specification | 4,577 | 1,260 | 1,145 |
| `exec` description only | 4,353 | 1,162 | 1,089 |
| `exec` Lark grammar only | 31 | 12 | 8 |
| Top-level `wait` specification | 438 | 107 | 110 |
| `wait` description only | 151 | 40 | 38 |
| Top-level `instructions` request field | 8,735 | 1,766 | 2,184 |
| `prompts/system.md` text only | 8,642 | 1,699 | 2,161 |
<!-- bcodex-tool-context:stable:end -->

The `exec` description contains the Code Mode runtime instructions and every
nested tool declaration. This is the text-only breakdown:

<!-- bcodex-tool-context:sections:start -->
| Section inside `exec` | UTF-8 bytes | o200k | bytes/4 |
| --- | ---: | ---: | ---: |
| Runtime and orchestration | 566 | 145 | 142 |
| Tool purposes and web policy | 1,800 | 445 | 450 |
| Defaults, limits, and process results | 355 | 108 | 89 |
| Schema-derived TypeScript | 1,626 | 464 | 407 |
<!-- bcodex-tool-context:sections:end -->

The full `exec` description is stored verbatim in
[`tool-catalogue.md`](tool-catalogue.md). A snapshot test compares that file to
the string produced by `src/tools/catalogue.rs`, and `bcodex --tool-catalogue`
prints the same generated string. Tool declarations still use Codex's
`render_json_schema_to_typescript` after recursively omitting descriptive JSON
Schema metadata. The fixed renderer removes insignificant TypeScript whitespace,
shares the repeated process result type, and emits one declaration block. Schema
property names, required/optional markers, arrays, enums, and return fields remain
generated from the source schemas. The full-description row is authoritative;
the section rows exclude their separator newlines.

## Description design and evaluation

OpenAI's current [GPT-5.6 model guidance](https://developers.openai.com/api/docs/guides/latest-model#favor-leaner-prompts)
says to remove repeated instructions and examples, keep tool descriptions
concise and precise, and rerun the same representative evaluations after each
change. The [function-calling guide](https://developers.openai.com/api/docs/guides/function-calling#token-usage)
confirms that tool definitions consume context and recommends shortening
descriptions while retaining clear purpose, parameters, formats, and outputs.
The compact catalogue follows that boundary: each instruction appears once;
input and output types remain complete; runtime defaults, process-state fields,
web routing, source selection, citations, and quotation limits remain explicit.
Only repeated headings, declaration wrappers, schema-comment prose, formatting
whitespace, and behavior irrelevant to bettercodex's fixed surface are absent.

Published evidence is directionally supportive but not treated as proof for
Sol. [EASYTOOL](https://aclanthology.org/2025.naacl-long.44/) found that
standardized concise tool instructions reduced tokens and improved tool-use
performance in its evaluated models and tasks. Conversely,
[Tool Preferences in Agentic LLMs are Unreliable](https://aclanthology.org/2025.emnlp-main.1060/)
found that description wording can strongly skew tool selection. That risk is
why `scripts/evaluate_tool_catalogue.py` freezes hard-graded local mutation,
session, image, planning, web-routing, citation, orchestration, wait, and helper
cases before comparing matched baseline and candidate binaries. Its acceptance
rule requires at least a 35% catalogue reduction with no lower aggregate or
per-case pass count; differing cases receive one additional matched repetition,
with all outcomes retained.

The frozen runner was used unchanged for this single-block renderer. An early
development run caught one candidate response exposing a native internal
citation marker (23/24 versus 24/24 baseline). The web contract was corrected
to make internal IDs and native markers explicitly call-only. Because that
changed model-visible text, the result below is a fresh full matrix after the
fix and the final input-format review, not a selective retry:

| Recorded measure | Previous catalogue | Single-block catalogue | Change |
| --- | ---: | ---: | ---: |
| Hard-graded passes | 24/24 | 24/24 | tied |
| Complete catalogue `o200k` estimate | 2,964 | 1,379 | -53.5% |
| Aggregate backend input tokens | 509,831 | 352,612 | -30.8% |
| Aggregate output tokens | 13,686 | 12,901 | -5.7% |
| Aggregate reasoning-output tokens | 6,095 | 6,014 | -1.3% |
| Aggregate wall time | 468.130 s | 420.131 s | -10.3% |

Every case tied 2/2, so the extra-repetition rule did not trigger. Aggregate
usage and timing differences are observations from this randomized low-sample
run, not performance claims. The complete 48-run artifact is
[`2026-08-07-single-block-ab.json`](../evaluations/tool-catalogue/2026-08-07-single-block-ab.json)
(SHA-256
`8b68a6fda4d62b15e650d779ef3bfed41218802c98703a6fcf7ee6e2320e1dc5`).

The frozen 2026-08-05 run predates this single-block renderer and is retained as
historical evidence for the preceding concise catalogue, not as validation of
the current change. It evaluated 12 cases twice per arm in randomized order.
Both catalogues passed every hard grade and every case was tied 2/2, so the
predeclared extra-repetition rule did not trigger. The candidate met the
catalogue-reduction threshold and the no-regression acceptance rules:

| Recorded measure | Previous catalogue | Concise catalogue | Change |
| --- | ---: | ---: | ---: |
| Hard-graded passes | 24/24 | 24/24 | tied |
| Complete catalogue `o200k` estimate | 5,195 | 2,939 | -43.4% |
| Median first-request backend input tokens | 6,106.5 | 3,992.5 | -34.6% |
| Aggregate backend input tokens | 613,340 | 539,250 | -12.1% |
| Aggregate output tokens | 16,658 | 18,083 | +8.6% |
| Aggregate reasoning-output tokens | 9,177 | 9,616 | +4.8% |
| Aggregate wall time | 544.538 s | 688.638 s | +26.5% |

The result does not justify claiming universal equivalence or a speedup. It is
one GPT-5.6 Sol run over 12 fixture tasks with two repetitions, and hard grades
measure observable task and tool behavior rather than subjective answer
quality. The concise arm was slower in 17 of 24 matched runs and made more web
verification calls, contributing to higher output, reasoning, and wall-time
totals despite lower input use. Those adverse measurements are retained rather
than tuned away or excluded. The complete unfiltered 48-run artifact, including
all outputs and tool calls, is
[`evaluations/tool-catalogue/2026-08-05-matched-ab.json`](../evaluations/tool-catalogue/2026-08-05-matched-ab.json)
(SHA-256
`a7c5e790a4bdbc3a2ba5eb3cf33e8d802dfa5b6c9febbbb649858e8757d759cb`).

The dynamic world-state items are not part of the tool specification, but they
occupy the same context window. With the default embedded `papercut` skill
implicitly invocable, they cost the following for the
bettercodex repository on the audit date:

<!-- bcodex-tool-context:dynamic:start -->
| Dynamic message item | UTF-8 bytes | o200k | bytes/4 |
| --- | ---: | ---: | ---: |
| Current `<environment_context>` developer item | 276 | 85 | 69 |
| Current `<repository_context>` user item | 5,835 | 1,377 | 1,459 |
| Current `<available_skills>` user item | 646 | 155 | 162 |
<!-- bcodex-tool-context:dynamic:end -->

Those rows are snapshots, not constants. The environment fields change with the
working directory, shell, date, and timezone. Repository instruction text
changes with the discovered files and is bounded to 64 KiB before the wrapper
is added. The skills item changes with discovered skills and operator settings;
its metadata lines use at most 2% of the effective context window and are capped
at 39,000 bytes.

## Top-level tools

Only two tools are visible directly to Sol.

### `exec`

`exec` is a custom freeform tool. Its `description` is exactly
[`tool-catalogue.md`](tool-catalogue.md). Its format is this exact Lark grammar:

```lark
start: SOURCE
SOURCE: /[\s\S]+/
```

This accepts the same nonempty source language as the previous grammar because
its `plain_source: SOURCE` branch already matched every character sequence. The
Rust `parse_exec_source` implementation remains responsible for recognizing and
validating an optional first-line pragma.

The description exposes seven nested tools through the JavaScript `tools`
object: `apply_patch`, `exec_command`, `log_papercut`, `update_plan`,
`view_image`, `write_stdin`, and the namespaced `web__run` (`web.run`). The web
tool uses Codex's exact command schema for search, open/fetch, click, find, PDF
screenshots, finance, weather, sports, time, and image search. Its concise
description preserves bettercodex's browsing, primary-source, citation, and
quotation rules without Codex's broad-product examples and repetition.

### `wait`

`wait` is a non-strict function tool. Its exact description is:

```text
Continue yielded `exec` by `cell_id`; returns only new output. Repeat while active; `terminate:true` stops. `yield_time_ms`/`max_tokens` default 10000.
```

Its exact input schema is:

```json
{
  "type": "object",
  "properties": {
    "cell_id": {"type": "string"},
    "yield_time_ms": {"type": "number"},
    "max_tokens": {"type": "number"},
    "terminate": {"type": "boolean"}
  },
  "required": ["cell_id"],
  "additionalProperties": false
}
```

## World-state message templates

The environment developer message uses this literal template:

```xml
<environment_context>
  <cwd>{XML-escaped working directory}</cwd>
  <shell>{XML-escaped SHELL, or /bin/bash}</shell>
  <current_date>{YYYY-MM-DD}</current_date>
  <timezone>{/etc/timezone, date +%Z, or unknown}</timezone>
</environment_context>
```

The repository-context user message uses this literal wrapper:

```xml
<repository_context>
<repository_instructions path="{XML-escaped path to the first applicable file}">
<![CDATA[
{trimmed file contents}
]]>
</repository_instructions>
<repository_instructions path="{XML-escaped path to the next applicable file}">
<![CDATA[
{trimmed file contents}
]]>
</repository_instructions>
</repository_context>
```

Discovery checks the Codex home directory first, then each directory from the
Git project root through the working directory. In each directory,
`AGENTS.override.md` replaces `AGENTS.md`.
An embedded `]]>` is split across adjacent CDATA sections so file contents
cannot terminate their structured field.

The skills-catalogue user message wraps bounded metadata lines in
`<available_skills>` markers. Skill-use rules live once in the harness contract,
not in every catalogue message. Only enabled skills whose
`allow_implicit_invocation` policy is true appear in that catalogue.
bettercodex's embedded `papercut` skill is materialized at the real
`${BCODEX_HOME:-$HOME/.bcodex}/skills/.system/papercut/SKILL.md` path; the model
reads its full body only after deciding to use it. Explicitly selected bodies
use separate `<skill_context>` user messages:

```xml
<skill_context>
<name>{XML-escaped skill name}</name>
<path>{XML-escaped SKILL.md path}</path>
<instructions><![CDATA[
{SKILL.md contents}
]]></instructions>
</skill_context>
```

As with repository context, embedded CDATA terminators are split so the body
cannot close its structured field. Catalogue metadata is XML-escaped.

## What is not model context

Responses Lite receives `code_mode_tool_names` in `x-codex-turn-metadata` and
client metadata. The map connects each normalized JavaScript name to its
canonical tool name and namespace. It is transport metadata, not an input item,
so it has no model-context token charge.

Codex conditionally adds standalone web search, `request_user_input`, MCP
tools, apps, plugins, image generation, dynamic namespaces, and multi-agent
tools. bettercodex deliberately fixes Codex's standalone `web.run` into its
catalogue and routes it to `alpha/search` with the same ChatGPT credentials,
live external access, direct-caller setting, session ID, model, bounded recent
conversation tail, and 10,000-token output budget. The other conditional tools
remain outside bettercodex until there is a concrete product use for them.
