Run JavaScript code to call tools and, where appropriate, orchestrate/compose tool calls
- Submit raw JavaScript source—not JSON, a quoted string, or a Markdown code fence. It runs in a fresh V8 isolate as an async module; the runtime has no Node, file system, network access, or console.
- All nested tools are available on the global `tools` object under normalized JavaScript identifiers. Call them as `await tools.exec_command(...)`; MCP names normalize to identifiers such as `await tools.mcp__ologs__get_profile(...)`.
- Nested tool methods accept a string or object and return an object or string, according to the tool description.
- Await every operation. When JavaScript finishes evaluating, the isolate ends and unawaited promises are silently discarded.
- Optional first line: `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 1000}`. `yield_time_ms` asks `exec` to yield early if the script is still running (default: 30000 ms); `max_output_tokens` sets the token budget for direct `exec` results (default: 10000 tokens).

Choose the smallest safe script:
- Use one nested call and return its complete output with `text(...)` or `image(...)` when one call is sufficient, its output is already small, its output shape is not reliably documented, fresh model judgment should follow, or the stage is adaptive, write/approval-sensitive, citation-heavy, or carries a native artifact. Do not batch, retry, filter, or chain it in JavaScript.
- Compose calls only for a bounded, predictable, read-only stage that reduces intermediate output to a materially smaller structured result. Use only documented calls and input/output fields needed by that stage. Define emitted fields and required evidence, call and retry limits, failure behavior, and stopping conditions; then return control to the model.
- Use `Promise.all(...)` only for independent, side-effect-free reads; keep dependent calls sequential. Nested failures reject their Promise; catch them only for explicit bounded recovery.

Global helpers:
- `exit()`: Immediately ends the current script successfully, like an early return from the top level.
- `text(value: string | number | boolean | undefined | null)`: Appends a text item; non-string values are stringified with `JSON.stringify(...)` when possible.
- `image(imageUrlOrItem: string | { image_url: string; detail?: "auto" | "low" | "high" | "original" | null } | ImageContent, detail?: "auto" | "low" | "high" | "original" | null)`: Appends an image item. `image_url` should be a base64-encoded `data:` URL. Forward an MCP tool image by passing an individual `ImageContent` block from `result.content`, for example `image(result.content[0])`. MCP image blocks may request detail with `_meta: { "codex/imageDetail": "original" }`; when provided, the second `detail` argument overrides detail embedded in the first argument.
- `generatedImage(result: { image_url: string; output_hint?: string })`: Appends an image-generation result and its optional output hint; HTTP(S) URLs are not supported.
- `store(key: string, value: any)`: Stores a serializable value under a string key for later `exec` calls in the same session.
- `load(key: string)`: Returns the stored value, or `undefined` if missing.
- `notify(value: string | number | boolean | undefined | null)`: Immediately injects an extra `custom_tool_call_output` for the current `exec` call; values are stringified like `text(...)`.
- `setTimeout(callback: () => void, delayMs?: number)`: Schedules a callback and returns a timeout id. Pending timeouts do not keep `exec` alive by themselves; await an explicit promise to wait for one.
- `clearTimeout(timeoutId?: number)`: Cancels a timeout created by `setTimeout`.
- `ALL_TOOLS`: Metadata for enabled nested tools as `{ name, description }` entries.
- `yield_control()`: Yields accumulated output to the model immediately while the script keeps running.

The following TypeScript blocks are `exec` tool declarations.

### `apply_patch`
The `apply_patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.

```ts
declare const tools: { apply_patch(input: string): Promise<unknown>; };
```

### `exec_command`
Runs a command in a PTY, returning output or a session ID for ongoing interaction.

```ts
type CommandResult = {
  // Chunk identifier included when the response reports one.
  chunk_id?: string;
  // Process exit code when the command finished during this call.
  exit_code?: number;
  // Approximate token count before output truncation.
  original_token_count?: number;
  // Command output text, possibly truncated.
  output: string;
  // Session identifier to pass to write_stdin when the process is still running.
  session_id?: number;
  // Elapsed wall time spent waiting for output in seconds.
  wall_time_seconds: number;
};

declare const tools: { exec_command(args: {
  // Shell command to execute.
  cmd: string;
  // True runs the shell with -l/-i semantics; false disables them. Defaults to true.
  login?: boolean;
  // Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy.
  max_output_tokens?: number;
  // Shell binary to launch. Defaults to the user's default shell.
  shell?: string;
  // True allocates a PTY for the command; false or omitted uses plain pipes.
  tty?: boolean;
  // Working directory for the command. Defaults to the turn cwd.
  workdir?: string;
  // Wait before yielding output. Defaults to 10000 ms; effective range is 250-30000 ms.
  yield_time_ms?: number;
}): Promise<CommandResult>; };
```

### `update_plan`
Updates the task plan.
Provide an optional explanation and a list of plan items, each with a step and status.
At most one step can be in_progress at a time.


```ts
declare const tools: { update_plan(args: {
  // Optional explanation for this plan update.
  explanation?: string;
  // The list of steps
  plan: Array<{
  // Step status.
  status: "pending" | "in_progress" | "completed";
  // Task step text.
  step: string;
}>;
}): Promise<unknown>; };
```

### `view_image`
View a local image file from the filesystem when visual inspection is needed. Use this for images already available on disk.

```ts
declare const tools: { view_image(args: {
  // Image detail level. Defaults to `high`; use `original` to preserve exact resolution.
  detail?: "high" | "original";
  // Local filesystem path to an image file.
  path: string;
}): Promise<{
  // Image detail hint returned by view_image. Returns `high` for default resized behavior or `original` when original resolution is preserved.
  detail: "high" | "original";
  // Data URL for the loaded image.
  image_url: string;
}>; };
```

### `write_stdin`
Writes characters to an existing unified exec session and returns recent output.

```ts
declare const tools: { write_stdin(args: {
  // Bytes to write to stdin. Defaults to empty, which polls without writing.
  chars?: string;
  // Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy.
  max_output_tokens?: number;
  // Identifier of the running unified exec session.
  session_id: number;
  // Wait before yielding output. Non-empty writes default to 250 ms and cap at 30000 ms; empty polls wait 5000-300000 ms by default.
  yield_time_ms?: number;
}): Promise<CommandResult>; };
```

## web
Tools in the web namespace.

### `web__run`
Tool for accessing the internet.

## Web research

Follow explicit user requests to browse or not browse. Otherwise, conduct deep research into current authoritative sources whenever external knowledge could improve the work beyond what the provided context and your built-in knowledge alone would surface. Use what you find to challenge assumptions, uncover better approaches, and re-evaluate conclusions; do not let your knowledge cutoff, initial framing, or prior confidence limit the result. For repository work, use external research to complement—not override—the source.

## Citations

`web.run` returns internal reference IDs such as `turn2search5`; use them only in later `web.run` calls, never in the final response.

Cite claims drawn from web research with direct Markdown links such as `[descriptive source title](https://example.com/page)`. Use a separate link for each source. Place citations near the claim they support, normally after sentence or paragraph punctuation. Do not use bare URLs, link to search-result pages, or place citations inside code fences, on standalone lines, or in an end-of-response list.

Each source must directly support the claim; clearly distinguish sourced facts from your own inferences. For technical claims, use only primary sources: official documentation, specifications, source code, or original research. Otherwise, prefer primary, authoritative sources; use different domains when multiple perspectives improve the response.


```ts
declare const tools: { web__run(args: {
  // Open links from previously opened pages.
  click?: Array<{
  // Numbered link id to open.
  id: number;
  // Reference id containing the numbered link.
  ref_id: string;
}>;
  // Find text patterns in pages.
  find?: Array<{
  // Text pattern to find.
  pattern: string;
  // Reference id or URL to search within.
  ref_id: string;
}>;
  // Query the image search engine for a given list of queries.
  image_query?: Array<{
  // Whether to filter by a specific list of domains.
  domains?: Array<string>;
  // Search query.
  q: string;
  // Whether to filter by recency, as a number of recent days.
  recency?: number;
}>;
  // Open pages by reference id or URL.
  open?: Array<{
  // Line number to position the page at.
  lineno?: number;
  // Reference id or URL to open.
  ref_id: string;
}>;
  // Response length; omit for `short`. Use `medium` or `long` when `search_query` contains 4 queries.
  response_length?: "short" | "medium" | "long";
  // Take screenshots of PDF pages.
  screenshot?: Array<{
  // Zero-indexed PDF page number.
  pageno: number;
  // Reference id or URL to screenshot.
  ref_id: string;
}>;
  // Query the internet; at most 4 queries per call.
  search_query?: Array<{
  // Whether to filter by a specific list of domains.
  domains?: Array<string>;
  // Search query.
  q: string;
  // Whether to filter by recency, as a number of recent days.
  recency?: number;
}>;
}): Promise<string>; };
```
