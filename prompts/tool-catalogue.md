Execute raw JavaScript to orchestrate tool calls.
- Input JavaScript directly, without JSON wrapping, quotes, or Markdown fences. A fresh V8 isolate supports top-level `await` but has no Node.js, filesystem, direct network access, console, or persistent global state.
- Call the typed methods below as `await tools.name(args)`. Use `Promise.all` for independent calls. Tool results are strings or the documented objects; await all work before the script ends.
- Emit output with `text(value)` or `image(dataUrlOrItem, detail?)`; `notify(value)` emits an interim tool output. `yield_control()` yields accumulated output while the script continues.
- `store(key, value)` and `load(key)` persist serializable values across exec cells. `exit()` finishes successfully. `setTimeout` and `clearTimeout` are available.
- An optional first line `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 1000}` controls early yielding and the direct-output token budget; defaults are 10000 for both.

### `apply_patch`
The `apply_patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.

exec tool declaration:
```ts
declare const tools: { apply_patch(input: string): Promise<unknown>; };
```

### `exec_command`
Runs a command in a PTY, returning output or a session ID for ongoing interaction.

exec tool declaration:
```ts
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
}): Promise<{
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
}>; };
```

### `log_papercut`
Appends one papercut note to `PAPERCUTS.md` at the Git repository root, creating the file on first use.

exec tool declaration:
```ts
declare const tools: { log_papercut(args: {
  // One or two sentences describing what caused friction and the likely fix when known.
  message: string;
}): Promise<{
  // Repository-relative path to the papercut log.
  path: string;
}>; };
```

### `update_plan`
Updates the task plan.
Provide an optional explanation and a list of plan items, each with a step and status.
At most one step can be in_progress at a time.

exec tool declaration:
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

exec tool declaration:
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

exec tool declaration:
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
}): Promise<{
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
}>; };
```

## web
Tools in the web namespace.

### `web__run`
Search and inspect live internet sources.

Use this tool when the user asks to browse (and do not use it when they ask not
to), when information may have changed (>10% chance) or is uncertain, for
recommendations involving substantial time or money or for high-stakes
accuracy, when exact links or quotations are needed, or when a referenced page,
paper, dataset, PDF, or site was not provided. For news, compare publication
dates with event dates.

For OpenAI-product questions, inspect local code first and otherwise use only
official OpenAI sites unless the user requests different sources. For technical
questions, use primary sources such as official documentation and research
papers. Prefer authoritative sources generally and label inferences.

Batch independent operations in one call. Omit unused fields and nulls.
`search_query` accepts at most four queries; more than three requires
`response_length` of `medium` or `long`.

Cite web-supported claims with direct, descriptive Markdown links placed next
to the claim. Do not cite search-result pages, use bare URLs as citations, or
expose internal result IDs such as `turn2search5`. Every citation must directly
support its claim; use multiple source domains when useful.

Respect each source's `[wordlim N]`. Quote at most 25 words from each non-lyrical
source and at most 10 words of song lyrics. Reddit may be quoted at greater
length only in a linked Markdown blockquote. Do not reproduce full works or
long passages; otherwise summarize or paraphrase.

exec tool declaration:
```ts
declare const tools: { web__run(args: {
  // Open links from previously opened pages.
  click?: Array<{
  // Numbered link id to open.
  id: number;
  // Reference id containing the numbered link.
  ref_id: string;
}>;
  // Look up prices for the given stock symbols.
  finance?: Array<{
  // ISO 3166-1 alpha-3 country code, "OTC", or "" for cryptocurrency.
  market?: string;
  // Ticker symbol to look up.
  ticker: string;
  // Asset type to look up.
  type: "equity" | "fund" | "crypto" | "index";
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
  // Set the length of the response to be returned.
  response_length?: "short" | "medium" | "long";
  // Take screenshots of PDF pages.
  screenshot?: Array<{
  // Zero-indexed PDF page number.
  pageno: number;
  // Reference id or URL to screenshot.
  ref_id: string;
}>;
  // Query the internet search engine for a given list of queries.
  search_query?: Array<{
  // Whether to filter by a specific list of domains.
  domains?: Array<string>;
  // Search query.
  q: string;
  // Whether to filter by recency, as a number of recent days.
  recency?: number;
}>;
  // Look up sports schedules and standings.
  sports?: Array<{
  // Start date in YYYY-MM-DD format.
  date_from?: string;
  // End date in YYYY-MM-DD format.
  date_to?: string;
  // Sports function to call.
  fn: "schedule" | "standings";
  // League to look up.
  league: "nba" | "wnba" | "nfl" | "nhl" | "mlb" | "epl" | "ncaamb" | "ncaawb" | "ipl";
  // Locale for the lookup.
  locale?: string;
  // Number of games to return.
  num_games?: number;
  // Opponent to use with `team` when narrowing the lookup.
  opponent?: string;
  // Team to look up, using the common 3 or 4 letter alias used in broadcasts.
  team?: string;
  // Tool name for sports requests.
  tool?: "sports";
}>;
  // Get time for the given UTC offsets.
  time?: Array<{
  // UTC offset formatted like "+03:00".
  utc_offset: string;
}>;
  // Look up weather forecasts.
  weather?: Array<{
  // Number of days to return. Defaults to 7.
  duration?: number;
  // Location in "Country, Area, City" format.
  location: string;
  // Start date in YYYY-MM-DD format. Defaults to today.
  start?: string;
}>;
}): Promise<unknown>; };
```
