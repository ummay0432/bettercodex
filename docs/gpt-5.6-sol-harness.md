# GPT-5.6 Sol harness mechanics

Checked on 2026-08-04 against OpenAI's public documentation and upstream Codex commit
[`1669c2403f793d0230065397dfc25f52b844244e`](https://github.com/openai/codex/tree/1669c2403f793d0230065397dfc25f52b844244e).
The cited upstream areas were also drift-reviewed against `main` at
[`c8e255e7f8cd8b33f60c42078bd3e793c82ea345`](https://github.com/openai/codex/tree/c8e255e7f8cd8b33f60c42078bd3e793c82ea345).
The pinned commit remains the reproducible source for the claims below. Recheck
both public documentation and upstream source before porting behavior that may
have changed.

Keep these sources separate:

| Source | What it proves |
| --- | --- |
| OpenAI API documentation | Public `gpt-5.6-sol` request, response, and item contracts. |
| Pinned Codex source | What that Codex commit chose for its catalog, ChatGPT backend, and client runtime. |
| This repository and `AGENTS.md` | BetterCodex product choices and code that exists here. |

Do not use one source as proof for another. The public API's 1,050,000-token window, pinned Codex's bundled 272,000-token window, and BetterCodex's 372,000-token target are different facts.

## Verified public Sol behavior

OpenAI describes Sol as a coding and long-horizon agent model with better token efficiency and tool use than earlier GPT-5 models. These are [published model claims](https://openai.com/index/gpt-5-6/), not proof that a client preserves the inference loop correctly.

| Item | Public contract |
| --- | --- |
| Model | `gpt-5.6-sol`; the `gpt-5.6` alias currently routes to it. |
| Context | 1,050,000 tokens; maximum input 922,000; maximum output 128,000. |
| Knowledge and I/O | Knowledge cutoff 2026-02-16; text and image input; text output. Images sent with `original` or `auto` detail retain their original dimensions. |
| Effort | `none`, `low`, `medium`, `high`, `xhigh`, `max`; omitted effort defaults to `medium`. |
| Mode | `reasoning.mode: "pro"` is independent of effort. Pro is not a model slug and is not another name for `max`. |
| Continuity | GPT-5.6 defaults `reasoning.context` to `all_turns`; `auto` currently resolves to the same behavior. Earlier reasoning works only when the earlier response items are available. |
| Summaries | `reasoning.summary: "auto"` opts into the most detailed available safe summary. Summary text streams as `response.reasoning_summary_text.delta` and is retained in the reasoning item's `summary` array; raw reasoning tokens are not exposed. |
| API | OpenAI recommends Responses. Sol supports Responses, Chat Completions, and Batch; supported features include streaming, structured outputs, function calling, file and web search, image input, and prompt caching. |
| Tools | The model page lists `web_search`, `file_search`, `image_generation`, `code_interpreter`, `hosted_shell`, `apply_patch`, `skills`, `computer_use`, `mcp`, and `tool_search`. Availability still depends on the endpoint and account. |

See the [`gpt-5.6-sol` model page](https://developers.openai.com/api/docs/models/gpt-5.6-sol), [model guidance](https://developers.openai.com/api/docs/guides/model-guidance?model=gpt-5.6), [reasoning modes](https://developers.openai.com/api/docs/guides/reasoning#reasoning-mode), and [reasoning summaries](https://developers.openai.com/api/docs/guides/reasoning#reasoning-summaries).

With `store: false` or Zero Data Retention, encrypted reasoning is returned by
default. The older `include: ["reasoning.encrypted_content"]` request remains
accepted for compatibility. Retain every output item, including any assistant
message `phase`, then replay the items unchanged. `previous_response_id` is the
alternative while the response state remains available. Sending `all_turns`
without earlier response state does not preserve reasoning. See [Preserve
reasoning across calls](https://developers.openai.com/api/docs/guides/reasoning#preserve-reasoning-across-calls).

[Programmatic Tool Calling](https://developers.openai.com/api/docs/guides/tools-programmatic-tool-calling) runs a model-written JavaScript `program` in a fresh hosted V8 isolate with top-level `await`, but no Node.js, packages, direct network access, general filesystem, subprocesses, console, or persistent JavaScript state. OpenAI runs the program; the client runs only returned client-owned tool calls. Nested `function_call` items have their own `call_id` and a `caller.caller_id` identifying the program. Execute every returned call and return one `function_call_output` per call with that call's `call_id` and copied `caller`. Retain the final `program_output` and, with stateless replay, every program, reasoning, call, and output item in order. `allowed_callers` permits direct calls, programmatic calls, or both on functions, custom tools, MCP, `apply_patch`, local or hosted shell, and `code_interpreter`; predictable tools should declare `parameters` and `output_schema`.

[Responses multi-agent](https://developers.openai.com/api/docs/guides/responses-multi-agent) is a beta hosted protocol enabled by the `responses_multi_agent=v1` beta header and `multi_agent.enabled`. Its root is `/root`; it supplies six hosted collaboration actions and emits `multi_agent_call`, `multi_agent_call_output`, and `agent_message`. The service, not the client, executes those hosted calls. There is no fixed total-agent or tree-depth limit; `max_concurrent_subagents` defaults to the recommended value of three active subagent turns across the tree. It is not Codex's local multi-agent v2 implementation.

Hosted multi-agent implicitly enables independent server-side compaction for
each agent. It does not support `/responses/compact`, `reasoning.summary`, or
`max_tool_calls` while enabled.

- [WebSocket mode](https://developers.openai.com/api/docs/guides/websocket-mode) supports incremental input with `previous_response_id` and `store: false`, plus a `generate: false` warmup that returns a chainable response ID without model output. An active socket caches only its most recent response in memory; an uncached ID with `store: false` returns `previous_response_not_found`, and a failed continuation evicts its referenced ID. Fall back to full input when the prior response or connection is gone. Connections last at most 60 minutes, run one response at a time, and do not multiplex responses.
- [Prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching) on GPT-5.6 matches exact breakpoint prefixes. The implicit breakpoint is the latest user or tool message and does not fall back to an earlier unmarked prefix. Put stable instructions and tools first; use `prompt_cache_breakpoint: {"mode":"explicit"}` with request-wide `prompt_cache_options.mode: "explicit"` when later content changes. A prefix must render at least 1,024 tokens; the only current TTL is `30m`; and a request can create at most four writes. Keep a stable `prompt_cache_key`, but do not treat the key alone as a hit. GPT-5.6 cache writes cost 1.25 times uncached input; inspect `cached_tokens` and `cache_write_tokens`.
- [Compaction](https://developers.openai.com/api/docs/guides/compaction) emits an opaque compaction item. Public Responses supports server-side threshold compaction through `context_management`; a stateless client may drop items before the most recent compaction item, while a `previous_response_id` client must not prune manually. Stateless standalone `/responses/compact` instead returns a canonical next input window that must be passed through without pruning. The ChatGPT Codex route observed on 2026-08-04 serialized the opaque output as `compaction_summary`; Codex also accepts the `compaction` protocol name. Codex's internal remote-compaction-v2 trigger described below is a separate ChatGPT/backend path.

## Pinned Codex choices for Sol

The bundled catalog is a fallback snapshot, not a public model limit or a live account result. Codex merges `/models` data and, for ChatGPT authentication, uses a nonempty visible remote catalog as its source of truth. See pinned [`models.json`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/models-manager/models.json#L4-L64) and the [model manager](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/models-manager/src/manager.rs#L394-L450). No account catalog was queried for this file.

| Choice | Pinned value or behavior |
| --- | --- |
| Window | 272,000; Codex's 95% usable factor yields 258,400. |
| Auto-compaction | 90% of resolved context; 244,800 for the bundled window. |
| Reasoning | Default `low`; options through `max`, plus the Codex-client `ultra` option. Summaries are supported but the Sol catalog default is `none`. |
| Transport | `prefer_websockets: true`; `use_responses_lite: true`. |
| Tools | `tool_mode: "code_mode_only"`; freeform `apply_patch`; model supports parallel calls. |
| Delegation | `multi_agent_version: "v2"`. |
| Other | Low verbosity, text/image web search, text/image input, original image detail, and a 10,000-token tool-output truncation policy. |

The context calculations are in [`openai_models.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/protocol/src/openai_models.rs#L355-L470). They do not verify a 372,000-token backend window.

Codex `ultra` is a product/client choice, not a public API effort. Pinned Codex maps `ultra` to wire effort `max` and changes local delegation from explicit-only to proactive; it does not send public pro mode. See [`client.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/client.rs#L175-L180) and [`multi_agents.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/session/multi_agents.rs#L39-L67). BetterCodex `max` therefore means maximum public effort, not pro mode or Codex's proactive `ultra` preset.

Responses Lite explicitly sends `all_turns`, requests encrypted reasoning, keeps additional tools and developer instructions first, uses a session cache key, and disables direct parallel tool calls. Codex prefers WebSockets and can prewarm the stable request with `generate: false`. It reuses a response only when every non-input property matches and the new input equals the prior input plus output followed by a delta; that delta may be empty after prewarm. It can retry over HTTPS. See [request construction](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/client.rs#L816-L929), [delta checks](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/client.rs#L1179-L1258), [prewarm](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/client.rs#L1525-L1777), and [retry logic](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/responses_retry.rs#L1-L68). Responses Lite is a Codex/backend path, not a public Sol capability.

The pinned Codex TUI can extract the first bold heading from a streamed reasoning-summary section and use it as the activity header, but the pinned Sol catalog disables summaries by default. BetterCodex deliberately enables the documented summary stream so that behavior is active rather than dormant. See pinned [`streaming.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/tui/src/chatwidget/streaming.rs#L229-L297).

Codex Code Mode is not public PTC. Codex exposes one client-owned freeform `exec` tool and runs JavaScript in its own V8 runtime. It generates nested IDs, bounds results, and derives TypeScript declarations from tool schemas; it does not use public `program`, `program_output`, and `caller` items. See the [`code_mode_only` plan](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/tools/spec_plan.rs#L560-L681), [Code Mode execution](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/tools/code_mode/mod.rs#L223-L410), and the [V8 runtime](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/code-mode-runtime/src/runtime/mod.rs#L73-L150). Port that observed ChatGPT path unless a wire trace proves public PTC works there.

### Codex tool audit

The pinned Sol entry selects `tool_mode: "code_mode_only"`. On Linux, Codex's stable `unified_exec` feature replaces `shell_command` with `exec_command` and `write_stdin`. The tool plan then has three distinct layers:

| Layer | Tools |
| --- | --- |
| Code Mode entrypoints | `exec` and `wait`; these are the only fixed request tools in BetterCodex. |
| Fixed nested catalogue | `apply_patch`, `exec_command`, `update_plan`, `view_image`, and `write_stdin`. Codex renders their descriptions and JSON schemas as TypeScript declarations inside the `exec` description. |
| Conditional Codex additions | Standalone or hosted web search, `request_user_input`, local multi-agent v2, image generation, MCP tools and resources, apps/connectors, tool search and plugin installation, extension tools such as goals, permissions, deferred environments, clock/sleep, token-budget utilities, and dynamic namespaces depend on provider capabilities, account state, feature/config state, or installed integrations. They are not a fixed Sol catalogue. |

BetterCodex ports the two entrypoints, all five fixed local nested tools, and—by explicit product choice—the conditional standalone `web.run` tool. [`tool-catalogue.md`](../prompts/tool-catalogue.md) is the exact generated text sent as the `exec` description; `bcodex --tool-catalogue` prints the same text. [`tool-context.md`](../prompts/tool-context.md) records the enclosing request items and their token-cost estimates. Responses Lite also receives the normalized nested-name map in `code_mode_tool_names`. The command schema omits Codex's approval and sandbox override fields because BetterCodex runs every command with the invoking user's permissions.

An installed Codex CLI 0.146.0 `ALL_TOOLS` trace on 2026-08-04 confirmed the five fixed nested names alongside account/config additions such as web, image generation, MCP resources, apps, plugin installation, and goal tools. The trace is evidence for the layering, not a portable catalogue: changing installed integrations changes those additional entries. BetterCodex's web port follows the pinned [standalone extension](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/ext/web-search/src/tool.rs) and [`alpha/search` client](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/codex-api/src/endpoint/search.rs), rather than exposing the incompatible hosted Responses tool.

The standalone extension's dedicated web-search begin/end activity, namespace handling, and `Searched the web` TUI cell were unchanged on upstream `main` at `2999b8e83132fd9f20a29696629dc0b4df259565` when rechecked on 2026-08-04.

Codex multi-agent v2 is also local. Client tools spawn other Codex threads and can fork no turns, all turns, or the latest N turns. See its [`spawn` handler](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs). Do not mix those tools with hosted Responses multi-agent items.

Other inference-loop mechanics worth porting exactly:

- History records completed API items immediately in API order. Its [normalizer](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/context_manager/normalize.rs#L20-L220) inserts stable synthetic failures for calls missing outputs and removes orphan outputs. Parallel tool futures use [`FuturesOrdered`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/session/turn.rs#L2107-L2220), so results are recorded in call order rather than completion order.
- Active-context accounting starts from the latest backend `total_tokens`, adds locally appended items, and adds earlier encrypted reasoning unless the transport reported `X-Reasoning-Included`. See [`history.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/context_manager/history.rs#L263-L315) and the [SSE header mapping](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/codex-api/src/sse/responses.rs#L28-L85).
- Tool inputs are closed typed shapes, but ordinary upstream function-tool serialization defaults `strict` to `false`. Code Mode uses output schemas for generated declarations; final structured text is strict. Do not claim every Codex tool sends API `strict: true`.
- Compaction runs before turns and after tool continuations. When the provider supports it, the stable, default-enabled remote-compaction-v2 path advertises `remote_compaction_v2`, appends a `compaction_trigger` to an ordinary streamed `/responses` request, and requires exactly one opaque compaction output. It keeps the newest real-user or hook prompts and non-completion agent messages under an approximate 64,000 text-token budget; an agent message estimated above 10,000 tokens is dropped, not capped or truncated. It rewrites trailing tool outputs when needed to fit the request and treats pre-turn and mid-turn context reinjection differently. See [`compact.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/compact.rs#L59-L74), [`compact_remote_v2.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/compact_remote_v2.rs#L383-L585), and [`compact_remote_v2_attempt.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/compact_remote_v2_attempt.rs#L30-L134). These compaction files were unchanged on upstream `main` at `c8e255e7f8cd8b33f60c42078bd3e793c82ea345` when rechecked on 2026-08-04.
- The unified executor supports persistent `exec_command`/`write_stdin` sessions, cancellation, bounded yields, a 10,000-token default output limit, a one-MiB retained-byte ceiling, and stable head/tail truncation. See [`unified_exec`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/unified_exec/mod.rs#L64-L74).

The BetterCodex port uses Codex's shell detection, fixed unified-exec
environment, yield bounds, output schemas, token estimator, head/tail
truncation, 64-session soft cap and LRU pruning, post-exit output-close grace,
and Code Mode result shapes. A sole text result uses Codex's plain-string wire
shape; structured results remain content-item arrays. Tool-output images pass
through Codex's validation and high/original resize limits before history
insertion. Non-TTY commands have closed stdin; interactive input requires
`tty: true`. `apply_patch` is a local-filesystem adaptation of the pinned
`codex-apply-patch` parser and applier because the upstream crate's public
boundary pulls in Codex's remote filesystem, sandbox, and exec-server layers.
It retains Codex's lenient grammar, fuzzy matching, sequential
partial-application behavior, and change summary.

## BetterCodex decisions and code

| Setting | BetterCodex target | Current code |
| --- | --- | --- |
| Model | `gpt-5.6-sol` | Implemented. |
| Reasoning | `max` | Implemented; neither pro nor Codex ultra. |
| Reasoning summary | `auto` | Implemented for ordinary and compaction turns so WebSocket request properties remain reusable. |
| Window | 372,000 | Product choice; no public or pinned-Codex verification. |
| Auto-compaction | 353,400, or 95% | Implemented as the fixed effective-window threshold. |

Implemented locally:

- [`src/api.rs`](../src/api.rs) sends `max`, `all_turns`, `reasoning.summary: "auto"`, `store: false`, encrypted reasoning, low verbosity, and the internal Responses Lite header. It validates any returned model ID and effective reasoning context. It prefers one WebSocket, sends guarded `previous_response_id` deltas only when the logical request is a strict extension, retries a missing connection-local response with full input, and falls back to HTTP SSE when upgrade support is unavailable.
- The stable tools and System-prompt prefix have an explicit cache breakpoint and a stable per-session cache key. Unsupported explicit-cache fields are removed and retried once. Every ordinary response that supplies usage records input, output, reasoning, cache-read, and cache-write token counts in the session journal; a zero count remains zero rather than being described as a cache hit.
- [`src/compaction.rs`](../src/compaction.rs), [`src/api.rs`](../src/api.rs), and [`src/context.rs`](../src/context.rs) implement remote compaction v2 as a state transition at 353,400 tokens. The normal Responses transport sends the trigger with the same cache key and request prefix, validates one opaque output, and keeps the newest real-user and non-completion agent messages under an approximate 64,000 text-token budget; agent messages estimated above 10,000 tokens are dropped. It removes stale generated context and reinjects current world state after a pre-turn compact or above the last real user/agent message for a mid-turn compact. The Codex model-visible token estimator, including encrypted reasoning and image/audio adjustments, drives fallback estimates; active usage honors `X-Reasoning-Included` and otherwise restores omitted prior reasoning tokens across resume. Oversized trailing tool outputs are rewritten only in the compaction request. History normalization repairs missing call outputs with stable IDs, removes orphan outputs, and preserves Code Mode's interim `notify()` outputs plus its final `exec` result.
- [`src/rollout.rs`](../src/rollout.rs) keeps a private append-only JSONL session journal with stable installation, session, and thread IDs. Resume supports an explicit session ID or the latest session for the current canonical working directory, repairs a torn final record, recovers unfinished turns, and retains usage and compaction state.
- [`src/agent.rs`](../src/agent.rs) has no model-step cap. Cancellation remains active during sampling, tools, compaction, and retry delays; steering can interrupt a stream or join a later continuation while a tool is running. Fresh input is projected into the compaction check, and queued steering waits through the first post-compaction tool continuation. Recoverable stream failures preserve completed items and return a model-visible continuation notice.
- [`src/tui/reasoning_status.rs`](../src/tui/reasoning_status.rs) incrementally extracts a bounded, sanitized Sol reasoning-summary heading. Each streamed summary section replaces the shimmered `Working` activity label; interruption and compaction override it, while active-tool detail remains appended.
- `AGENTS.override.md` and `AGENTS.md` are loaded from the Codex home and project root through the working directory with one file per directory and a shared 64-KiB bound. [`src/input.rs`](../src/input.rs) accepts repeated local PNG, JPEG, WEBP, and GIF inputs with the four GPT-5.6 detail values and a 50-MiB aggregate byte limit.
- The mock Responses harness covers HTTP SSE, WebSocket deltas, connection-local recovery, HTTPS fallback, explicit-cache fallback, usage fields, output ordering, and remote-v2 compaction request, retention, and output validation. ChatGPT-backed one-shot turns, including a live Sol reasoning-summary item with the expected bold activity heading, and process-boundary resume were exercised on 2026-08-04; the earlier unary compact-endpoint fixture applied to the replaced legacy implementation.
- Code Mode is the local Codex mechanism, not public PTC. The top-level `exec` program composes the fixed nested catalogue, including persistent `exec_command`/`write_stdin` sessions, under bounded output and cancellation. An enclosing `exec` or `wait` result defaults to 10,000 tokens.
- [`src/web_search.rs`](../src/web_search.rs) exposes the canonical `tools.web__run` mapping and sends its search and fetch/navigation commands to `https://chatgpt.com/backend-api/codex/alpha/search`. It shares refreshed ChatGPT authentication with Responses, sends Codex's live/direct settings, current model/session/turn metadata, bounded recent text context, and 10,000-token output budget, and uses the pinned Codex retry policy and opaque-result response shape. Nested activity retains the `web.run` namespace boundary and uses Codex's `Searching the web` / `Searched the web for …` terminal cell instead of exposing the model-facing result payload in the transcript.

Known boundaries and discrepancies:

- The 372,000 raw window is a BetterCodex product value rather than backend metadata.
- BetterCodex does not send the public/upstream WebSocket `generate: false` prewarm. Its delta guard requires a nonempty strict extension.
- Compaction response usage is parsed but not written to the session journal.
- A nested `exec_command` or `write_stdin` call with no explicit `max_output_tokens` exposes its full bounded one-MiB retained buffer to JavaScript before the enclosing Code Mode result is truncated. The generated nested-tool description currently says the nested default itself is 10,000 tokens; that is not what the executor implements.
- The embedded runtime uses Codex's pinned cell/session implementation with the standard cross-platform `rusty_v8` archive; it does not link Codex release builds' target-specific pointer-compression and V8-sandbox artifacts. This does not expose Node.js, filesystem, network, or console globals.
- There is no public PTC, hosted multi-agent beta, or local delegation surface; add delegation only for a concrete BetterCodex workflow. Keep wire effort `max`, and do not describe it as pro mode or Codex ultra.
