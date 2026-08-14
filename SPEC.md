# Direct tool stack

## Status

Implemented on August 14, 2026. This document records the product decision and
the retained contracts of the resulting direct tool stack.

## Decision

bettercodex diverges from upstream Codex and does not use Code Mode. The model
receives four fixed ordinary functions:

- `bash`
- `read`
- `write`
- `edit`

The same Responses request also declares hosted `web_search`, configured for
live text and image results. Local images are read through `read`, as they are
in pi, rather than requiring a second filesystem-reading tool. There is no
reason to put a JavaScript runtime between the model and four fixed functions.

## Why

The former path made even one shell command or file edit pass through `exec`
and a nested tool call. bettercodex carried roughly 6,500 lines of V8 Code Mode
runtime and 11,500 lines under `src/tools/` for a small, fixed catalogue. That
machinery makes sense for upstream's MCP, apps, plugins, dynamic tools, and
large tool ecosystem. bettercodex deliberately has none of those.

That runtime was client-side Codex Code Mode, not the Responses API's hosted
Programmatic Tool Calling feature.

ChatGPT subscription authentication does not require Responses Lite. A live
August 14, 2026 probe showed that the Lite route rejects
`parallel_tool_calls: true` with `unsupported_value`, while pi uses the same
ChatGPT OAuth backend without the Lite header and enables native parallel tool
calls. Do not send the Responses Lite header or its `additional_tools` input
framing. Use the normal Responses contract instead: put the harness prompt in
top-level `instructions`, expose the four ordinary function schemas and compact
hosted-search declaration through top-level `tools`, and set
`parallel_tool_calls: true`.

When one response contains multiple function calls, start independent calls
concurrently and retain their outputs in model call order. `bash` and `read`
support concurrent execution. Keep `write` and `edit` exclusive so a
side-effecting direct file operation does not overlap another direct function
call. Hosted web search executes within the Responses service rather than the
local function dispatcher.
This mirrors Codex's shared/exclusive dispatch gate without retaining its
general tool-router machinery.

`update_plan` was also removed. It did not store or enforce execution state; it
only validated and rendered a model-authored checklist that was then duplicated
in conversation context. Controlled todo-tool ablations on frontier
models, including GPT-5.6 Terra, found no statistically significant accuracy
gain and generally lower cost without it, so its context, latency, and tool-use
overhead are not justified.

## Tool contracts

- `read(path, offset?, limit?, detail?)` reads bounded UTF-8 text or returns a
  local image as an image attachment. Detect images from their bytes, not their
  filename. `offset` and `limit` apply only to text; `detail` applies only to
  images and supports `high` or `original`.
- `write(path, content)` creates a file or completely replaces one, creating
  parent directories when needed.
- `edit(path, edits)` applies one or more exact, unique text replacements to one
  file atomically.
- `bash(command, timeout?)` runs one command in the working directory and
  returns bounded stdout, stderr, and exit status.
- hosted `web_search` searches and browses the live web using text and image
  results; its `web_search_call` items remain in Responses history and saved
  sessions.

`view_image` is folded into `read`; there is no standalone tool. The direct
implementation reuses the retained image validation, 50 MiB input limit,
high/original preparation, and history handling. The `read` output schema must
describe its actual direct output: either a text string or an image content
list. User-supplied `-i` attachments remain unchanged.

The former standalone `/alpha/search` client, `web_run` schema, reference-ID
protocol, and local dispatch path are not retained. Native `url_citation`
annotations are preserved with assistant messages and rendered as visible,
clickable source links. The backend does not document a native equivalent for
the former explicit PDF-page screenshot command; do not recreate it through a
fallback function or dynamic tool route.

## Process lifecycle

Use pi's `bash` contract rather than Codex's unified exec contract. It starts a
non-PTY shell child, streams stdout and stderr to the UI, and waits for it to
exit. It has no default timeout; an explicit timeout or interruption kills the
process tree. It has no background sessions, session IDs, or later stdin. Use
`tmux` when a persistent process is needed.

Keep Codex's reliable process-group cleanup, environment scrubbing, and bounded
model output. The stateful yield, polling, PTY, and stdin protocol are not
retained.

## Removed by the migration

- model-visible `exec` and `wait`;
- `update_plan`, its events, and plan-specific TUI rendering;
- the V8 runtime, JavaScript bridge, nested-tool catalogue, and their
  dependencies;
- `apply_patch` and `exec_command`;
- standalone `view_image`;
- standalone `web_run`, its `/alpha/search` client, schema, description, and
  local result protocol; and
- Code Mode-only prompt, history, recovery, and test machinery.

`write_stdin`, background-process management, and their TUI surfaces were also
removed.

Keep the ordinary Responses `function_call` / `function_call_output` loop,
hosted `web_search_call` history, saved-session fidelity, citation annotations,
cancellation, output bounds, file safety, image implementation, and supported
macOS/Linux behavior.

Native Windows support is intentionally removed as part of this simplification.
Do not restore Windows builds, installers, release assets, dependencies,
process/runtime branches, terminal handling, documentation, or tests.

## Boundaries

Do not add dedicated `grep`, `find`, or `ls` tools; `bash` already covers them.
Do not add MCP, dynamic tool search, hosted Programmatic Tool Calling, or a
Code Mode fallback. The four direct functions plus fixed hosted `web_search`
are the complete model-visible catalogue.

The August 14, 2026 live compatibility spikes established both sides of the
transport decision. GPT-5.6 Sol accepted direct `bash` and image-reading tools
through Responses Lite, but the backend rejected a Lite request with
`parallel_tool_calls: true` and the exact error
`X-OpenAI-Internal-Codex-Responses-Lite requires parallel_tool_calls to be
false.` A follow-up request using the same ChatGPT subscription, normal
top-level Responses tools, and `parallel_tool_calls: true` completed with HTTP
200 and emitted both requested independent function calls in one response,
matching pi's transport. The replacement and transport switch were implemented
as one migration rather than maintaining two tool systems or a Lite fallback.
