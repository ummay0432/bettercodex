# GPT-5.6 Sol harness mechanics

Checked on 2026-08-04 against OpenAI's public documentation and upstream Codex commit
[`1669c2403f793d0230065397dfc25f52b844244e`](https://github.com/openai/codex/tree/1669c2403f793d0230065397dfc25f52b844244e).
Recheck both before porting behavior that may have changed.

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
| Effort | `none`, `low`, `medium`, `high`, `xhigh`, `max`; omitted effort defaults to `medium`. |
| Mode | `reasoning.mode: "pro"` is independent of effort. Pro is not a model slug and is not another name for `max`. |
| Continuity | GPT-5.6 defaults `reasoning.context` to `all_turns`, but earlier reasoning works only when the earlier response items are supplied. |
| Summaries | `reasoning.summary: "auto"` opts into the most detailed available safe summary. Summary text streams as `response.reasoning_summary_text.delta` and is retained in the reasoning item's `summary` array; raw reasoning tokens are not exposed. |
| API | OpenAI recommends Responses. Sol supports structured outputs, function calls, streaming, prompt caching, and built-in tools. |

See the [`gpt-5.6-sol` model page](https://developers.openai.com/api/docs/models/gpt-5.6-sol), [model guidance](https://developers.openai.com/api/docs/guides/model-guidance?model=gpt-5.6), [reasoning modes](https://developers.openai.com/api/docs/guides/reasoning#reasoning-mode), and [reasoning summaries](https://developers.openai.com/api/docs/guides/reasoning#reasoning-summaries).

With `store: false`, encrypted reasoning is returned by default. The older
`include: ["reasoning.encrypted_content"]` request remains accepted for
compatibility. Retain every output item and its `phase`, then replay them
unchanged. `previous_response_id` is the alternative while the response
remains available. Sending `all_turns` without earlier response state does not
preserve reasoning. See [Preserve reasoning across calls](https://developers.openai.com/api/docs/guides/reasoning#preserve-reasoning-across-calls).

[Programmatic Tool Calling](https://developers.openai.com/api/docs/guides/tools-programmatic-tool-calling) runs a model-written JavaScript `program` in a fresh hosted V8 isolate with no Node.js, filesystem, or network access except exposed tools. Nested `function_call` items have their own `call_id` and a `caller.caller_id` identifying the program. Return exactly one result with the same `call_id` and copied `caller`, retain the final `program_output`, and replay every item. Tools use `allowed_callers` to permit direct calls, programmatic calls, or both; predictable tools should declare `parameters` and `output_schema`.

[Responses multi-agent](https://developers.openai.com/api/docs/guides/responses-multi-agent) is a beta hosted protocol enabled by `responses_multi_agent=v1` and `multi_agent.enabled`. It emits `multi_agent_call`, `multi_agent_call_output`, and `agent_message`. It is not Codex's local multi-agent v2 implementation.

- [WebSocket mode](https://developers.openai.com/api/docs/guides/websocket-mode) supports incremental input with `previous_response_id` and `store: false`. Fall back to full input when the prior response or connection is gone. Connections last at most 60 minutes and do not multiplex responses.
- [Prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching) matches exact prefixes. Put stable instructions and tools first, append changing items, keep a stable `prompt_cache_key`, and inspect `cached_tokens` and `cache_write_tokens`. A cache key alone proves nothing.
- [Compaction](https://developers.openai.com/api/docs/guides/compaction) emits an opaque compaction item. Public Responses supports server-side threshold compaction through `context_management` and stateless standalone compaction through `/responses/compact`; the latter returns a canonical next input window that must not be pruned. The ChatGPT Codex route observed on 2026-08-04 serialized the opaque output as `compaction_summary`; Codex also accepts the `compaction` protocol name. Codex's internal remote-compaction-v2 trigger described below is a separate ChatGPT/backend path.

## Pinned Codex choices for Sol

The bundled catalog is a fallback snapshot, not a public model limit or a live account result. Codex merges `/models` data and, for ChatGPT authentication, uses a nonempty visible remote catalog as its source of truth. See pinned [`models.json`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/models-manager/models.json#L4-L64) and the [model manager](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/models-manager/src/manager.rs#L394-L450). No account catalog was queried for this file.

| Choice | Pinned value or behavior |
| --- | --- |
| Window | 272,000; Codex's 95% usable factor yields 258,400. |
| Auto-compaction | 90% of resolved context; 244,800 for the bundled window. |
| Reasoning | Default `low`; options through `max`, plus Codex-only `ultra`. Summaries are supported but the Sol catalog default is `none`. |
| Transport | `prefer_websockets: true`; `use_responses_lite: true`. |
| Tools | `tool_mode: "code_mode_only"`; freeform `apply_patch`; model supports parallel calls. |
| Delegation | `multi_agent_version: "v2"`. |
| Other | Low verbosity, text/image web search, text/image input, 10,000-token tool-output truncation policy. |

The context calculations are in [`openai_models.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/protocol/src/openai_models.rs#L355-L470). They do not verify a 372,000-token backend window.

Codex `ultra` is a client choice, not a public API effort. Pinned Codex maps `ultra` to wire effort `max` and changes local delegation from explicit-only to proactive; it does not send public pro mode. See [`client.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/client.rs#L175-L180) and [`multi_agents.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/session/multi_agents.rs#L39-L67). BetterCodex `max` therefore means maximum public effort, not pro and not four-agent ultra.

Responses Lite explicitly sends `all_turns`, requests encrypted reasoning, keeps additional tools and developer instructions first, uses a session cache key, and disables direct parallel tool calls. Codex prefers WebSockets and sends a delta only when every non-input property matches and new input strictly extends the prior input plus output. It can retry over HTTPS. See [request construction](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/client.rs#L816-L929), [delta checks](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/client.rs#L1180-L1220), and [retry logic](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/responses_retry.rs#L1-L68). Responses Lite is a Codex/backend path, not a public Sol capability.

The pinned Codex TUI can extract the first bold heading from a streamed reasoning-summary section and use it as the activity header, but the pinned Sol catalog disables summaries by default. BetterCodex deliberately enables the documented summary stream so that behavior is active rather than dormant. See pinned [`streaming.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/tui/src/chatwidget/streaming.rs#L229-L297).

Codex Code Mode is not public PTC. Codex exposes one client-owned freeform `exec` tool and runs JavaScript in its own V8 runtime. It generates nested IDs, bounds results, and derives TypeScript declarations from tool schemas; it does not use public `program`, `program_output`, and `caller` items. See the [`code_mode_only` plan](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/tools/spec_plan.rs#L560-L681) and [Code Mode runtime](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/tools/code_mode/mod.rs#L223-L410). Port that observed ChatGPT path unless a wire trace proves public PTC works there.

### Codex tool audit

The pinned Sol entry selects `tool_mode: "code_mode_only"`. On Linux, Codex's stable `unified_exec` feature replaces `shell_command` with `exec_command` and `write_stdin`. The tool plan then has three distinct layers:

| Layer | Tools |
| --- | --- |
| Code Mode entrypoints | `exec` and `wait`; these are the only fixed request tools in BetterCodex. |
| Fixed nested catalogue | `apply_patch`, `exec_command`, `update_plan`, `view_image`, and `write_stdin`. Codex renders their descriptions and JSON schemas as TypeScript declarations inside the `exec` description. |
| Conditional Codex additions | Standalone or hosted web search, `request_user_input`, local multi-agent v2, image generation, MCP tools, apps, plugins, and dynamic namespaces depend on provider capabilities, account state, feature/config state, or installed integrations. They are not a fixed Sol catalogue. |

BetterCodex ports the two entrypoints, all five fixed local nested tools, and—by explicit product choice—the conditional standalone `web.run` tool. [`tool-catalogue.md`](../prompts/tool-catalogue.md) is the exact generated text sent as the `exec` description; `bcodex --tool-catalogue` prints the same text. [`tool-context.md`](../prompts/tool-context.md) records the enclosing request items and their token-cost estimates. Responses Lite also receives the normalized nested-name map in `code_mode_tool_names`. The command schema omits Codex's approval and sandbox override fields because BetterCodex runs every command with the invoking user's permissions.

An installed Codex CLI 0.146.0 `ALL_TOOLS` trace on 2026-08-04 confirmed the five fixed nested names alongside account/config additions such as web, image generation, MCP resources, apps, and plugin installation. The trace is evidence for the layering, not a portable catalogue: changing installed integrations changes those additional entries. BetterCodex's web port follows the pinned [standalone extension](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/ext/web-search/src/tool.rs) and [`alpha/search` client](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/codex-api/src/endpoint/search.rs), rather than exposing the incompatible hosted Responses tool.

Codex multi-agent v2 is also local. Client tools spawn other Codex threads and can fork no turns, all turns, or the latest N turns. See its [`spawn` handler](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs). Do not mix those tools with hosted Responses multi-agent items.

Other inference-loop mechanics worth porting exactly:

- History records completed API items immediately in API order. Its [normalizer](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/context_manager/normalize.rs#L20-L220) inserts stable synthetic failures for calls missing outputs and removes orphan outputs. Parallel tool results are recorded in call order.
- Tool inputs are closed typed shapes, but ordinary upstream function-tool serialization defaults `strict` to `false`. Code Mode uses output schemas for generated declarations; final structured text is strict. Do not claim every Codex tool sends API `strict: true`.
- Compaction runs before turns and after tool continuations. The stable, default-enabled remote-compaction-v2 path advertises `remote_compaction_v2`, appends a `compaction_trigger` to an ordinary streamed `/responses` request, requires exactly one opaque compaction output, and retains at most 64,000 tokens of recent real-user and non-completion agent messages. It caps each retained agent message at 10,000 tokens, rewrites trailing tool outputs when needed to fit the request, and treats pre-turn and mid-turn context reinjection differently. See [`compact.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/compact.rs#L59-L74), [`compact_remote_v2.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/compact_remote_v2.rs#L383-L585), and [`compact_remote_v2_attempt.rs`](https://github.com/openai/codex/blob/1669c2403f793d0230065397dfc25f52b844244e/codex-rs/core/src/compact_remote_v2_attempt.rs#L30-L134). These compaction files were unchanged on upstream `main` at `d1f14e31a7000e7361599e057dc401077a4b5a05` when rechecked on 2026-08-04.
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

- [`src/api.rs`](../src/api.rs) sends `max`, `all_turns`, `reasoning.summary: "auto"`, `store: false`, encrypted reasoning, low verbosity, and the internal Responses Lite header. It prefers one WebSocket, sends guarded `previous_response_id` deltas only when the logical request is an exact extension, retries a missing connection-local response with full input, and falls back to HTTP SSE when upgrade support is unavailable.
- The stable tools and System-prompt prefix have an explicit cache breakpoint and a stable per-session cache key. Unsupported explicit-cache fields are removed and retried once. Every ordinary turn response records input, output, reasoning, cache-read, and cache-write token counts in the session journal; a zero count remains zero rather than being described as a cache hit.
- [`src/compaction.rs`](../src/compaction.rs), [`src/api.rs`](../src/api.rs), and [`src/context.rs`](../src/context.rs) implement remote compaction v2 as a state transition at 353,400 tokens. The normal Responses transport sends the trigger with the same cache key and request prefix, validates one opaque output, retains Codex's bounded recent-message window, removes stale generated context, and reinjects current world state after a pre-turn compact or above the last real user/agent message for a mid-turn compact. The Codex model-visible token estimator, including encrypted reasoning and image/audio adjustments, drives fallback estimates and oversized trailing tool outputs are rewritten only in the compaction request. History normalization repairs missing call outputs with stable IDs, removes orphan outputs, and preserves Code Mode's interim `notify()` outputs plus its final `exec` result.
- [`src/rollout.rs`](../src/rollout.rs) keeps a private append-only JSONL session journal with stable installation, session, and thread IDs. Resume supports an explicit session ID or the latest session for the current repository, repairs a torn final record, recovers unfinished turns, and retains usage and compaction state.
- [`src/agent.rs`](../src/agent.rs) has no model-step cap. Cancellation remains active during sampling, tools, compaction, and retry delays; steering can interrupt a stream or join a later continuation while a tool is running. Fresh input is projected into the compaction check, and queued steering waits through the first post-compaction tool continuation. Recoverable stream failures preserve completed items and return a model-visible continuation notice.
- [`src/tui/reasoning_status.rs`](../src/tui/reasoning_status.rs) incrementally extracts a bounded, sanitized Sol reasoning-summary heading. Each streamed summary section replaces the shimmered `Working` activity label; interruption and compaction override it, while active-tool detail remains appended.
- `AGENTS.override.md` and `AGENTS.md` are loaded from the Codex home and project root through the working directory with a single 64-KiB bound. [`src/input.rs`](../src/input.rs) accepts repeated local PNG, JPEG, WEBP, and GIF inputs with the four GPT-5.6 detail values.
- The mock Responses harness covers HTTP SSE, WebSocket deltas, connection-local recovery, HTTPS fallback, explicit-cache fallback, usage fields, output ordering, and remote-v2 compaction request, retention, and output validation. ChatGPT-backed one-shot turns, including a live Sol reasoning-summary item with the expected bold activity heading, and process-boundary resume were exercised on 2026-08-04; the earlier unary compact-endpoint fixture applied to the replaced legacy implementation.
- Code Mode is the local Codex mechanism, not public PTC. The top-level `exec` program composes the fixed nested catalogue, including persistent `exec_command`/`write_stdin` sessions, under bounded output and cancellation.
- [`src/web_search.rs`](../src/web_search.rs) exposes the canonical `tools.web__run` mapping and sends its search and fetch/navigation commands to `https://chatgpt.com/backend-api/codex/alpha/search`. It shares refreshed ChatGPT authentication with Responses, sends Codex's live/direct settings, current model/session/turn metadata, bounded recent text context, and 10,000-token output budget, and uses the pinned Codex retry policy and opaque-result response shape.

Remaining boundaries are intentional. The 372,000 raw window is a BetterCodex product value rather than backend metadata. The embedded runtime uses Codex's pinned cell/session implementation with the standard cross-platform `rusty_v8` archive; it does not link Codex release builds' target-specific pointer-compression and V8-sandbox artifacts. This does not expose Node.js, filesystem, network, or console globals. There is no public PTC, hosted multi-agent beta, or local delegation surface; add delegation only for a concrete BetterCodex workflow. Keep wire effort `max`, and do not describe it as pro mode or Codex ultra.
