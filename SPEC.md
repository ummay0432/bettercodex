# Initial prompt (verbatim)

```text
the "streaming" overhaul of agent text into terminal is incomplete and flawed, for example with filechanges:  this was just plopped down into the terminal, all at once. Thats not really consistent with the "streaming" thing we're doing is it?

I propose you refactor it so that it shows like "Editing 'filename.tsx' (+31 -2) or something, and it shows the diff in realtime, as the agent is editing it. Does that make sense? Same should apply to when its making/writing new files,

I feel like this as a concept should also be carried over to tool usage! right now tool usage isnt shown until the command/tool is finished being written, which takes away from the cohesiveness of the text the agent writes being streamed, does that make sense? If we could see the commands it writes in real time, at the exact pace the terminal is receiving it from the model, so character per character, with NO Latency! I think this would make bcdoex feel way better.

Basically: apply what we did to the agent text streaming, to everything else in the terminal

Dont implement this yet, do brainstorming with me on how you propose we do this, what the best and most satsifying implementation would be, with the least amount of latency between the character the model writes, and what is shown in the terminal, i think that'd be cool.

So SPEC.md write it in there, your proposal. include this initial prompt in it at the top. Do research.
```

# Follow-up prompt (verbatim)

```text
For example rght, i just want to chime in for a second. dont let this interrupt your work.

"• Searched the web for site:platform.openai.com/docs/api-reference/responses websocket response.create generate false OpenAI ..."

This happened in a chat, but it was pretty much teleported into the chat, it wasnt written into the terminal. If we could see the model write whatever its doing in realtime, it'd be much more satisfying UX for the terminal. DOes that give you a decent idea of what we're going for? This should apply to everything the agent does, in the terminal.
```

# Live agent authorship and tool streaming

Status: proposal only; no runtime behavior is implemented by this document.

## Executive proposal

bettercodex should make the model's act of composing every terminal-visible action observable. A
web search, command, patch, plan, or other tool call must not first appear as a completed block when
an earlier model-input or runtime-output delta was available.

The cohesive implementation is an end-to-end streaming pipeline with four layers:

1. Preserve tool-input deltas from the Responses stream instead of waiting for a completed output
   item.
2. Feed assistant text and tool input through one ordered, timestamped presentation scheduler.
3. Incrementally project the freeform Code Mode JavaScript into truthful, purpose-specific live
   views such as a command, web query, or patch, without ever executing incomplete source.
4. Stream runtime output and lifecycle transitions through that same scheduler, so a fast tool
   cannot go from absent to completed between terminal frames.

The intended result is not a typing animation attached after the fact. It is a live view of bytes
that have actually arrived from the model or a running process. The only unavoidable delay is the
next terminal frame. There must be no fixed debounce, no 500 ms batching window, and no artificial
sleep between characters.

## Product principle

> If bettercodex has received a safe-to-display prefix of an agent action, the next eligible
> terminal frame should display that prefix. Completion is a state transition on an already-visible
> action, not the moment the action is introduced.

This applies to everything the agent does in the terminal:

- assistant prose;
- the streamed reasoning heading;
- Code Mode source when no richer projection is possible;
- shell commands and terminal interactions;
- web search, open, click, find, image-search, and PDF screenshot operations;
- patches that add, update, delete, move, or create files;
- plan updates;
- image paths and other generic tool arguments;
- subprocess stdout/stderr or PTY output;
- long-running tool progress, waits, warnings, and completion transitions.

Session replay, static startup content, operator-authored slash commands, and already-completed
history should render immediately rather than replaying an animation.

## The concrete failure mode

Today a fast web operation can first render as:

```text
• Searched the web for site:platform.openai.com/docs/api-reference/responses ...
```

That line looks as if it teleported into the transcript. The desired lifecycle is:

```text
• Searching the web for s
• Searching the web for site:platform.openai.com/
• Searching the web for site:platform.openai.com/docs/api-reference/responses
• Searched the web for site:platform.openai.com/docs/api-reference/responses
```

Each line above represents successive frames of the same mutable live entry, not four transcript
entries. The final past-tense form replaces the active form only after the operation completes.

The same pattern applies to commands:

```text
• Writing command pnpm t
• Writing command pnpm test --filter tui
• Running pnpm test --filter tui
  └ test output arrives here while the process runs
• Ran pnpm test --filter tui
  └ final output
```

And to edits:

```text
• Editing 'src/components/App.tsx' (+2 -1)
    41 - const title = "Old";
    41 + const title = "New";
    42 + const subtitle = "Streaming";
```

For a new file:

```text
• Creating 'src/components/Status.tsx' (+3)
     1 + export function Status() {
     2 +   return <span>Ready</span>;
     3 + }
```

The current, incomplete diff row should itself grow as its characters arrive. Addition and removal
counts update in place. A completed action becomes `Edited`, `Created`, `Deleted`, or `Moved`; a
failed or interrupted action must say that its preview was not applied.

## Research findings

### Responses supports the missing data

The official
[`Responses streaming events`](https://developers.openai.com/api/reference/resources/responses/streaming-events)
contract includes both `response.function_call_arguments.delta` and
`response.custom_tool_call_input.delta`, with an item ID, output index, sequence number, and input
delta. Current upstream Codex also parses the custom-tool event into a tool-input event in
[`codex-api/src/sse/responses.rs`](https://github.com/openai/codex/blob/72fa74fbc9c4b72d513304bfa0eda427d2402ed9/codex-rs/codex-api/src/sse/responses.rs#L365-L375).

bettercodex currently timestamps and forwards `response.output_text.delta`, but its response event
switch does not forward either custom-tool input deltas or function-call argument deltas. Tool calls
are collected only from authoritative completed output items. This is the first source of the
teleportation.

Both HTTPS/SSE and WebSocket responses already pass through the same response-event processor, so
the delta work belongs in that shared processor. It must not be implemented separately per
transport.

### Code Mode changes where the preview must happen

bettercodex deliberately exposes only the top-level Responses tools `exec` and `wait`. Commands,
patches, web operations, plans, and other tools are nested Code Mode calls inside the model-authored
freeform JavaScript. Consequently, upstream's direct `apply_patch` streaming path cannot simply be
connected to the TUI: bettercodex first has to observe the streamed `exec` source and extract direct
nested calls from it.

This extraction is for presentation only. The V8 runtime must continue to receive and execute only
the complete, validated source. Streaming must never turn into speculative execution.

### Upstream has a patch-parser foundation, but not the target UX

Current upstream Codex includes a line-oriented
[`StreamingPatchParser`](https://github.com/openai/codex/blob/72fa74fbc9c4b72d513304bfa0eda427d2402ed9/codex-rs/apply-patch/src/streaming_parser.rs)
and emits structured `PatchApplyUpdated` events from custom-tool input. That parser should be ported
and kept aligned with the retained apply-patch grammar instead of inventing a second patch grammar.

The upstream presentation path is not sufficient for this proposal:

- it is behind `ApplyPatchStreamingEvents`;
- it buffers updates for 500 ms in
  [`apply_patch.rs`](https://github.com/openai/codex/blob/72fa74fbc9c4b72d513304bfa0eda427d2402ed9/codex-rs/core/src/tools/handlers/apply_patch.rs#L53-L54);
- it emits only after complete patch lines;
- its update conversion clones the accumulated change set; and
- current upstream TUI code does not render `PatchApplyUpdated`.

bettercodex should port the parser and its coverage, not the latency or cloning policy. Always-on
streaming is the intentional bettercodex product behavior; no configuration switch is needed.

### Upstream also has runtime output deltas

Current Codex emits bounded `ExecCommandOutputDelta` events while stdout/stderr or PTY output is
read. The relevant implementations are
[`core/src/exec.rs`](https://github.com/openai/codex/blob/72fa74fbc9c4b72d513304bfa0eda427d2402ed9/codex-rs/core/src/exec.rs#L1127-L1140)
and
[`core/src/unified_exec/async_watcher.rs`](https://github.com/openai/codex/blob/72fa74fbc9c4b72d513304bfa0eda427d2402ed9/codex-rs/core/src/unified_exec/async_watcher.rs#L36-L50).
The current upstream TUI does not provide a complete model for rendering those deltas, but the core
event shape and UTF-8/bounding rules are the retained precedent to port.

### The current TUI has a lifecycle race

bettercodex's current assistant presentation queue is a strong base: it records receive time,
preserves grapheme clusters, paces frames at up to 120 FPS, and prevents tool events from overtaking
queued assistant text.

Discrete tool lifecycle events do not yet receive equivalent treatment. A `ToolStarted` and
`ToolCompleted` pair can both be drained and applied before the next draw. The first visible frame
then renders the entry in past tense, even though a start event was available. This explains the
reported `Searched the web ...` teleport independently of model-input streaming. Both the upstream
delta gap and this frame-coalescing race must be fixed.

### Patch and process output are currently whole-value displays

The TUI currently builds `PatchDisplay` by parsing the complete nested `apply_patch` input when the
runtime emits `ToolStarted`. It parses the full patch and reads source previews synchronously, so
the first patch frame necessarily contains the whole diff.

Likewise, `ProcessSession` receives output chunks continuously, but `ProcessManager` returns a
snapshot only after the command exits or yields. The TUI sees output only in `ToolCompleted`. A
cohesive streaming product should expose those already-available process chunks too.

## Alternatives considered

### Animate a completed tool call after the fact

Rejected. Splitting an already-complete command or patch into a typing animation would look smooth,
but it would add latency and misrepresent when the data arrived. The proposal uses real model
deltas and only applies bounded frame pacing to an already-received burst.

### Show the tool only at runtime start

Insufficient. This can make a slow-running tool visibly active, but it still hides the model writing
the query/command/patch. It also does not fix fast start/completion pairs that collapse before a
draw.

### Expose every nested tool directly to Responses

Rejected. That would abandon bettercodex's fixed Code Mode-only route and change model-facing tool
contracts merely for presentation. Streaming must observe the retained `exec` source instead.

### Use a full JavaScript parser or execute partial source

Rejected. A parser cannot resolve general dynamic values, a new parser is unnecessary dependency
surface, and partial execution is unsafe. A bounded lexical projector plus an immediate raw-source
fallback is both more truthful and simpler.

### Copy upstream patch progress unchanged

Rejected. The parser is valuable, but the 500 ms buffer, complete-line-only display, accumulated
state cloning, and absent TUI integration conflict with the zero-intentional-delay requirement.

## Terminology and truthfulness

The implementation must distinguish two different streams:

1. **Authorship stream**: bytes the model is generating for text or tool input.
2. **Execution stream**: lifecycle and output events from an actually running tool.

The UI must not imply that a side effect occurred merely because the model drafted it. Recommended
states are:

```text
Receiving -> Ready -> Running -> Succeeded
    |          |         |          |
    +----------+---------+------> Failed / Interrupted / Not run
```

- `Receiving`: the model is still writing the call.
- `Ready`: the complete call exists, but runtime execution has not started.
- `Running`: the nested tool has actually started.
- `Succeeded`/`Failed`: authoritative runtime completion.
- `Interrupted`/`Not run`: a visible draft never executed or execution was cancelled.

The visual copy can remain natural and compact. A shimmer/activity marker differentiates an active
entry from a completed bullet. For patches, `Editing`/`Creating` is acceptable while receiving as
long as interruption is rendered explicitly as `Edit not applied`/`File not created`. For commands,
`Writing command` is clearer than `Running` before execution starts.

## Proposed architecture

### 1. Represent model tool-input streaming explicitly

Add in-process agent events equivalent to:

```rust
ModelToolCallStarted {
    stream_id,
    item_id,
    output_index,
    call_id,
    name,
    kind,
    received_at,
}
ModelToolCallInputDelta {
    stream_id,
    item_id,
    delta,
    received_at,
}
ModelToolCallInputCompleted {
    stream_id,
    item_id,
    call_id,
    name,
    input,
    received_at,
}
ModelToolExecutionStarted { call_id, received_at }
ModelToolExecutionCompleted { call_id, received_at }
ToolOutputDelta { call_id, stream, chunk, received_at }
```

The exact Rust names can follow the existing event style, but the distinctions must remain.

Behavior by Responses event:

- `response.output_item.added`: create the draft immediately using item metadata. Use the official
  item ID as the primary streaming identity, then validated call ID/output index fallbacks for
  compatible streams that omit it.
- `response.custom_tool_call_input.delta`: append raw freeform `exec` input.
- `response.function_call_arguments.delta`: append raw JSON arguments for `wait` and any retained
  function-call path.
- `response.custom_tool_call_input.done` / `response.function_call_arguments.done`: reconcile the
  accumulated prefix and mark authorship ready without waiting for a later response-level event.
- `response.output_item.done`: reconcile against the authoritative complete input, finish the draft,
  and keep the completed response item/history behavior unchanged.
- response attempt failure/retry: close or replace drafts belonging to the failed stream ID so an
  old attempt cannot absorb a later attempt's deltas.

`response.output_item.done` remains authoritative. If the concatenated deltas differ from the final
item, the UI must correct its active draft before execution rather than letting presentation state
alter conversation history.

These are transient presentation events. Do not persist every delta in JSONL or duplicate partial
tool inputs in model history. The completed Responses item and completed nested-tool transcript
remain the durable representation.

### 2. Generalize assistant presentation into an ordered authorship scheduler

Replace the assistant-only queue plus discrete-item side queue with one ordered presentation
timeline. It should carry:

- assistant-text fragments;
- reasoning-heading fragments;
- model tool-input fragments;
- tool start/progress/output/completion transitions;
- warnings and other terminal-visible agent events.

Every fragment carries its network/runtime `received_at` and an ordering sequence. Adjacent deltas
for the same item may be coalesced in memory without a timer, retaining the earliest timestamp.
Coalescing must never reorder across item or lifecycle boundaries.

The scheduler should preserve these rules:

1. Never reveal a character before it was received.
2. Never let a later tool event overtake earlier assistant text or tool input.
3. Reveal Unicode extended grapheme clusters atomically; “character by character” must not split an
   emoji, combining mark, or other user-perceived character.
4. Request the earliest frame allowed by the existing 120 FPS ceiling.
5. Do not debounce, sleep, or wait for punctuation, JSON completion, a newline, or tool completion
   when a safe prefix can already be displayed.
6. Under backlog, increase the reveal batch just as the current assistant scheduler does rather
   than accumulating visible latency.
7. A newly created action must be drawn at least once before a terminal completion transition can
   replace it. If start and completion arrive in one event-loop drain, show running on the first
   eligible frame and completion on the next. This is a one-frame state barrier, not a fake minimum
   tool duration.

The display target is event-to-next-frame streaming, not a claim that the API delivers one event per
character. Responses deltas are often token-sized or larger. bettercodex can reveal the graphemes in
an arriving delta smoothly, but it cannot display a character before the server sends the delta
containing it.

Recommended latency budgets:

- no fixed batching/debounce interval anywhere in the path;
- first safe grapheme visible on the next eligible frame (8.34 ms frame ceiling; allow two frames
  as the practical p95 manual target);
- ordinary received-prefix backlog below 50 ms;
- abnormal large-delta catch-up bounded by the existing 200 ms policy;
- completion never allowed to erase the only visible active frame.

The metric should be captured from API `received_at` to successful terminal draw in debug/test
instrumentation, not from model token creation time, which the client cannot observe.

### 3. Add an incremental, non-executing Code Mode projector

The top-level `exec` input is arbitrary JavaScript. It is impossible to recover every nested tool
call statically: arguments may be computed, calls may be conditional, and loops may invoke one
source expression more than once. The design must acknowledge that instead of pretending to be a
full JavaScript evaluator.

Add a small incremental lexical projector for direct calls. It observes the displayed source prefix
and recognizes patterns such as:

```javascript
await tools.exec_command({cmd: "cargo test"})
await tools.apply_patch(`*** Begin Patch\n...`)
await tools.web__run({search_query: [{q: "Responses API"}]})
```

Requirements for the projector:

- handle arbitrary delta boundaries, including boundaries inside identifiers, UTF-8 text, escape
  sequences, comments, string delimiters, and nested delimiters;
- recognize and skip the optional leading `// @exec: ...` pragma without confusing it for a nested
  call;
- understand single-quoted, double-quoted, and template string literals;
- decode a literal incrementally for semantic display while retaining the exact raw source;
- recognize normalized Code Mode names such as `web__run` and map them to display names such as
  `web.run`;
- support direct object/array literals deeply enough to stream known display fields;
- stop semantic projection at dynamic expressions or template interpolation it cannot prove, then
  fall back cleanly rather than guessing;
- perform no filesystem mutation, network request, V8 execution, or general constant evaluation;
- process each incoming byte a bounded number of times; never reparse the full source every frame.

Purpose-specific adapters consume recognized literal fields:

| Nested tool | Live projection |
| --- | --- |
| `exec_command` | `cmd`, then optional workdir/TTY detail |
| `write_stdin` | target process and characters being written/waited for |
| `apply_patch` | per-file operation, live counts, and live diff |
| `web__run` | search/image queries, URLs, links, find patterns, PDF pages |
| `update_plan` | plan steps and status as they are authored |
| `view_image` | path and detail |
| `log_papercut` | compact action label; do not expose irrelevant object syntax |
| generic namespaced nested tool | normalized name plus a bounded raw argument preview |

When a richer semantic projection exists, it should replace the noisy orchestration boilerplate in
the main transcript. If the projector cannot understand a direct call, show the `exec` source as a
live `Writing code` cell. Never hide all activity while waiting for complete JavaScript.

This fallback is essential for truthfulness. “Everything streams” means every received source has
a live representation, not that every arbitrary program can be perfectly reverse-engineered.

### 4. Reconcile drafts with actual nested runtime calls

Projected nested calls do not initially have runtime call IDs. Give them presentation-only IDs based
on the model stream/item plus lexical occurrence. When Code Mode emits an actual `ToolStarted`,
match it to the earliest unbound compatible draft using:

- normalized tool name;
- exact string equality for freeform input;
- deep value equality for function input; and
- source occurrence/FIFO as a tie-breaker.

On a match, transition the existing entry to `Running` and attach the real call ID. Do not insert a
second tool entry. If there is no safe match—because the input was computed, a loop invoked the call,
or parsing fell back—create the normal runtime entry immediately.

When the top-level `exec` call completes:

- matched drafts use their actual outcomes;
- unmatched syntactic previews that never ran disappear if they were still only an active overlay,
  or finalize explicitly as `Not run` if any part was already committed to transcript history;
- dynamically generated runtime calls remain normal tool entries;
- the final durable transcript contains actual nested calls, not duplicate speculative previews.

The agent/runtime should emit top-level execution start/end boundaries so the TUI knows when it is
safe to resolve unmatched previews. This is cleaner than inferring execution completion from the
next model response.

### 5. Port and extend incremental patch parsing

Port current upstream `StreamingPatchParser` into the retained local patch implementation, along
with its arbitrary-split, character-split, CRLF, malformed-input, add/update/delete/move, and final
line coverage. Keep the streaming and final parser on one grammar.

Adapt the interface for bettercodex's hot path:

- expose append operations or borrowed current state instead of cloning every accumulated hunk on
  every delta;
- parse complete lines once;
- separately expose the current incomplete line as a provisional visual row so the row text itself
  streams before newline;
- update per-file `+N`/`-N` counts incrementally;
- use the final complete parser result as the canonical reconciliation point;
- preserve existing patch preview limits and sanitization.

Recommended copy:

| Patch kind/state | Active | Success | Failure/interruption |
| --- | --- | --- | --- |
| add | `Creating 'path' (+N)` | `Created 'path' (+N)` | `File not created 'path'` |
| update | `Editing 'path' (+N -M)` | `Edited 'path' (+N -M)` | `Edit not applied 'path'` |
| delete | `Deleting 'path' (-N)` | `Deleted 'path' (-N)` | `File not deleted 'path'` |
| move | `Moving 'old' → 'new' (+N -M)` | `Moved 'old' → 'new' (+N -M)` | `Move not applied` |

Counts should appear as soon as known and update in place. A delete may initially show `(-?)` until
a bounded source read supplies the original line count.

#### Source line numbers and filesystem reads

Do not put repeated filesystem reads on the per-character TUI path. Show patch-relative numbers and
content immediately. At the first complete file header, request at most one bounded source snapshot
for that path; when available, upgrade the live row numbers using the existing source-location
logic. Cache that snapshot for the draft and use the pre-apply version.

If asynchronous source loading would make the implementation disproportionately complex, a single
bounded read matching the current 2 MiB total preview budget is acceptable, but it must occur only
once per file and never once per frame/delta.

#### Mutable live region versus terminal scrollback

Terminal scrollback cannot be edited. A long in-flight patch therefore needs an explicit live-region
policy:

- keep mutable headers, counters, and the provisional final row in the Ratatui-controlled viewport;
- retain a bounded head/tail of live diff rows and an omitted-row count;
- do not spill a row into terminal history until it is immutable;
- on completion, commit one canonical final patch card using the existing bounded preview policy.

This prevents a count or partial line in scrollback from disagreeing with the final patch. It also
keeps resize/reflow deterministic.

### 6. Stream process output at capture time

`ProcessSession` already receives stdout/stderr/PTY chunks. Extend that capture path to emit a
bounded `ToolOutputDelta` immediately after the chunk is accepted into the same ordered process
buffer used for the final snapshot.

Requirements:

- use one ordering sequence shared with the merged final output so live and final order agree;
- retain stdout/stderr identity for piped commands and `Pty`/combined identity for terminal mode;
- split only on valid UTF-8 boundaries, carrying an incomplete byte sequence to the next chunk;
- sanitize control sequences with state across chunk boundaries before rendering;
- cap event count and live retained bytes using current upstream output-delta bounds as the starting
  precedent;
- preserve the existing complete model-visible output/truncation contract;
- deduplicate at completion—the final snapshot reconciles the live buffer and must not append the
  same output a second time;
- support initial `exec_command` and later `write_stdin`/wait windows against the same process.

An extremely chatty process must not starve keyboard input or cancellation. Coalesce adjacent ready
output deltas without a timer and retain the current event-loop fairness cap. Rendering remains at
the frame ceiling even if capture produces thousands of chunks per second.

### 7. Make every lifecycle transition frame-aware

All mutable entries should share a presentation lifecycle rather than directly mutating transcript
state in event-drain order. The minimum contract is:

- `Receiving` must be visible before `Ready` if any authored prefix was received;
- `Running` must be visible before `Succeeded`/`Failed` for a runtime-only action that had no draft;
- an already-visible `Receiving` entry may transition through `Ready` to `Running` in one frame if
  no useful visual difference would be lost;
- completion waits at most until the next eligible frame, never a human-visible artificial delay;
- activity details below the busy status and the transcript entry derive from the same state so they
  cannot disagree;
- final tense and color come only from authoritative completion.

This directly fixes the web-search example even before considering how long the actual search
takes.

## Surface behavior matrix

| Surface | While model writes | While running | Completion |
| --- | --- | --- | --- |
| Assistant text | Existing grapheme stream | n/a | Stable Markdown |
| Reasoning heading | Stream heading prefix | Shimmer | Last valid heading/status |
| `exec` fallback | `Writing code` + source prefix | `Running code` | `Ran code`/failure |
| Command | Command prefix | Live command + output | `Ran` + reconciled output |
| Web | Query/URL/pattern prefix | `Searching`/`Opening`/`Finding` | Past tense, same entry |
| Patch update | File header, counts, diff chars | `Editing` | `Edited` or not applied |
| New file | Path and added rows | `Creating` | `Created` or not created |
| Delete/move | Path(s), counts | `Deleting`/`Moving` | Truthful terminal state |
| Plan | Steps/status prefixes | `Updating plan` | Stable plan |
| `write_stdin` | Input prefix | Live process output | `Interacted`/`Waited` |
| Generic tool | Name + bounded raw args | Tool activity | Success/failure |
| Warning/notification | n/a | Next frame | Stable notice |

## Ordering, retries, and cancellation

### Global ordering

Use the receive order from the shared API processor as the presentation order. The scheduler must
maintain boundaries between assistant output items, tool-call items, nested tool execution, and
follow-up model responses. A later completed tool must never jump above an earlier partially
presented message or draft.

### Retries and interrupted response streams

Associate drafts with a sampling stream/attempt ID in addition to item ID. On a retry:

- never merge the new attempt into the old draft solely because item IDs happen to match;
- mark a visible old draft interrupted or remove it if it never left the mutable overlay;
- keep completed output items/history behavior exactly as the inference recovery path requires;
- do not execute an incomplete tool draft.

### User cancellation

Esc must remain immediately responsive. On cancellation:

- reveal no additional cosmetic backlog before processing the interrupt;
- stop runtime work through the existing cancellation token;
- flush only enough presentation state to leave a truthful final line;
- label a drafted-but-unexecuted command or edit as not run/not applied;
- preserve already-received process output without fabricating a success state.

### Malformed or unsupported input

The raw source fallback is always available. A malformed partial patch or JSON object should not
flash an error while the model is still writing; partial syntax is expected. Report an error only
when the authoritative complete tool item/runtime rejects it. Until then, retain the last safe
semantic prefix plus the live raw fallback.

If a compatible endpoint sends only a completed item and no input deltas, render that completed
item immediately and truthfully. bettercodex cannot reconstruct the model's original timing, and
must not manufacture a delayed typing animation to conceal the endpoint limitation.

## Performance and resource constraints

The feature should improve perceived latency without making the TUI's hot path expensive.

- Parse each input byte incrementally; no whole-source parse on every delta or frame.
- Store one canonical raw source plus compact parser/projector state. Avoid duplicate full strings
  for raw input, decoded input, diff, and rendered lines where slices or append operations suffice.
- Cache rendered stable rows; rerender only the mutable tail, changed header, or width-dependent
  projection.
- Keep model/event ingress non-blocking and keep terminal input ahead of cosmetic catch-up.
- Bound raw argument previews, patch rows/source bytes, process output, and generic tool details.
- Coalesce without timers. “Wait 100/500 ms and emit the latest value” is specifically rejected.
- Preserve the 120 FPS ceiling; do not attempt one terminal draw per byte.
- Continue using synchronized terminal updates so a frame appears atomically.
- Preserve Windows UTF-8/path behavior and avoid assumptions that every command is POSIX shell.

No new JavaScript parser dependency is recommended. A full parser still cannot safely evaluate
dynamic arguments, would materially expand the dependency surface, and is unnecessary for direct
literal projections. Port the upstream patch parser into the existing local patch module rather
than adding a second patch crate.

## Security and trust boundaries

- Never execute partial model source or a projected argument.
- Treat all model text, tool input, filenames, command output, web strings, and errors as untrusted
  terminal content; use stateful control-sequence sanitization across deltas.
- Resolve/display paths relative to the session cwd using the existing path and hyperlink rules.
- Streaming changes presentation timing only. It must not weaken cancellation, tool validation,
  filesystem rules, or process cleanup.
- Do not persist transient deltas or speculative nested calls as if they ran.
- Do not expose hidden authentication material through new process inheritance or logging.

## Suggested code shape

This is a likely decomposition, not permission to add generic infrastructure without a caller:

- `src/api.rs`: recognize custom/function tool-input deltas, correlate output items, timestamp at
  ingress, and reconcile final items.
- `src/events.rs`: add explicit authored-tool, top-level execution, and tool-output events.
- `src/agent.rs` / `src/tools/mod.rs`: emit top-level execution boundaries without changing history.
- `src/tools/patch.rs`: port upstream streaming patch grammar and expose efficient incremental
  state.
- `src/tools/process_session.rs` / `src/tools/executor.rs`: emit ordered bounded output chunks from
  the existing capture path.
- `src/tui/presentation.rs`: generalize the scheduler and enforce state-transition frame barriers.
- a focused TUI module such as `src/tui/tool_stream.rs`: incremental Code Mode projection,
  draft/runtime reconciliation, and per-tool live state.
- `src/tui/view.rs`: render mutable tool entries and commit canonical completed entries.

Do not change model-facing prompts, tool descriptions, schemas, or startup art for this work. The
existing Code Mode contract already supplies the source needed for presentation.

## Implementation sequence

The work can be developed in stages, but should ship as one cohesive terminal behavior rather than
another half-streamed state.

### Stage 1: transport and lifecycle correctness

- Forward custom and function tool-input deltas with receive timestamps.
- Add response-attempt/item correlation.
- Generalize the presentation timeline.
- Add the one-visible-frame lifecycle barrier.
- Prove the fast web start/end case no longer teleports.

### Stage 2: live Code Mode projection

- Stream raw `exec` source immediately.
- Add the incremental direct-call projector.
- Implement command, web, generic tool, plan, image, wait, and interaction adapters.
- Reconcile projected drafts with runtime call IDs and outcomes.

### Stage 3: live file changes

- Port upstream `StreamingPatchParser` and nearest tests.
- Add provisional incomplete-line rendering and efficient append state.
- Integrate add/update/delete/move displays, counts, source line numbers, and bounded live history.
- Reconcile final patch input and runtime result.

### Stage 4: live runtime output

- Port upstream's output-delta principles into `ProcessSession`.
- Stream command and terminal output into existing entries.
- Preserve final snapshots, truncation, process polling, cancellation, and background sessions.

### Stage 5: hardening and polish

- Exercise retries, cancellation, malformed source, computed arguments, multiple direct calls,
  `Promise.all`, loops, large patches/output, resize/reflow, and native Windows behavior.
- Profile the hot path for accidental quadratic parsing/rendering.
- Manually validate the perceived flow in a real terminal, not only state-level tests.

## Validation strategy

Follow the existing bettercodex and nearest upstream coverage rather than creating a parallel test
harness.

### API/event tests

- custom-tool input deltas are emitted in order with item identity and receive timestamps;
- function-call argument deltas receive the same treatment;
- arbitrary event boundaries and WebSocket/HTTPS paths produce the same events;
- final input reconciliation catches a mismatched accumulated prefix;
- retry attempts cannot cross-associate drafts.

### Projector tests

- split at every byte of representative direct calls;
- strings, escapes, comments, Unicode graphemes, arrays/objects, and normalized tool names;
- direct command, patch, web query, plan, wait, and generic tool projections;
- computed arguments and template interpolation fall back without guessing;
- processing is append-only rather than reparsing the full source.

### Patch tests

- port current upstream streaming-parser tests first;
- add only bettercodex-observable coverage for provisional row text and live counters;
- streamed final projection equals the existing canonical completed patch rendering;
- add/update/delete/move, CRLF, no final newline, malformed completion, large rows, and bounded
  source previews.

### Rendered TUI tests

- a partial web query is visible before completion;
- `ToolStarted` and `ToolCompleted` drained together produce a running frame followed by a completed
  frame, never a first-frame completed teleport;
- command characters, patch rows, and new-file contents grow across deterministic frame advances;
- tool input cannot overtake preceding assistant text;
- resize/reflow preserves the same final transcript;
- completion does not duplicate live output;
- cancellation leaves truthful not-run/not-applied states;
- replay renders completed state immediately and persists no speculative drafts.

### Process tests

- stdout/stderr/PTY chunks become visible before process completion;
- split UTF-8 and split control sequences are handled safely;
- live order matches final snapshot order;
- output caps, yield, `write_stdin`, timeout, interrupt, and descendant cleanup remain intact.

### Manual acceptance scenarios

1. Ask for a long, distinctive web query. Observe the query grow before the search completes.
2. Ask for a multi-line command. Observe the command form, transition to running, and emit output.
3. Ask to update a file with many changed lines. Observe path, counts, and each diff row evolve.
4. Ask to create and then move a file. Observe `Creating` and `Moving` rather than a complete patch
   appearing at once.
5. Cancel while the model is halfway through a patch. Verify the UI says it was not applied and the
   filesystem is unchanged.
6. Run a tool that completes inside one frame. Verify it is first visible as active, then complete
   on the next frame.
7. Resize repeatedly during all of the above and verify no duplicate, reordered, or corrupted rows.

## Acceptance criteria

The overhaul is complete only when all of the following hold:

- No model-authored terminal action waits for its complete Responses output item when an input delta
  is available.
- The web-search example visibly forms before it becomes `Searched the web`.
- Direct nested commands visibly form before execution starts.
- File add/update/delete/move previews show a live path, live counts, and live diff text.
- Runtime command output appears before completion/yield.
- A start and completion received in one event-loop drain cannot first render as completed.
- There is no fixed debounce or artificial per-character sleep.
- First-prefix presentation is bounded by the next eligible terminal frame under normal load.
- Unicode, control-sequence filtering, ordering, cancellation, retries, persistence, resize/reflow,
  and native platform behavior remain correct.
- Dynamic JavaScript that cannot be projected still streams immediately as `Writing code` and never
  disappears into a blank wait.
- Completed transcript/history contains only authoritative assistant messages and actually executed
  tools, with no duplicate speculative entries.

## Explicit non-goals

- Executing a command or patch before the complete model tool call is validated.
- Pretending the API emits one network event per character.
- Building a general JavaScript evaluator or predicting dynamic runtime values.
- Animating resumed/completed session history.
- Adding configuration, providers, an app server, SDK, plugin/MCP infrastructure, or another binary.
- Changing tool schemas or model-facing instructions merely to make the UI easier to parse.
- Adding audio or video behavior.

## Recommendation

Implement the full pipeline, not a patch-only animation. The most satisfying result comes from one
shared authorship/lifecycle scheduler: assistant text, tool arguments, semantic tool views, runtime
output, and completion all become successive states of the same ordered stream.

The critical design choices are:

1. consume real Responses tool-input deltas;
2. preview direct Code Mode calls without executing them;
3. port the upstream streaming patch grammar but reject its 500 ms presentation buffer;
4. emit process output where it is captured;
5. force at least one visible active frame before completion; and
6. fall back to live raw code whenever semantic projection is not provably correct.

That combination gives bettercodex the intended feeling: the terminal is watching the agent work,
not receiving a transcript of work that already happened.
