# Tools

## `bash`

Execute a Bash command in the current working directory and wait for it to exit. Returns JSON with bounded `stdout` and `stderr` strings plus integer `exit_code`; runtime failures return plain error text. A timeout kills the process tree, and background children are terminated when the shell exits.

### Input

- `command: string` (required) — Bash command to execute.
- `timeout?: number` — Positive seconds before killing the process tree. Optional; no timeout by default.

### Output

- `stdout: string` (required) — Bounded standard output.
- `stderr: string` (required) — Bounded standard error.
- `exit_code: integer` (required) — Process exit status. Timeout returns 124 and interruption returns 130.

## `read`

Read bounded UTF-8 text or inspect a PNG, JPEG, GIF, or WebP image from a local file. Use `read` rather than shell commands to inspect a known file. Text reads stop at 39,000 bytes or the optional `limit` in lines, whichever comes first, and report the next offset when truncated. Image reads accept files up to 50 MiB and return one image attachment. Failures return plain error text.

### Input

- `path: string` (required) — Path to the file, relative to the working directory or absolute.
- `detail?: "high" | "original"` — Image files only: detail level. Defaults to `high`; use `original` to preserve exact resolution.
- `limit?: integer` — Text files only: maximum number of lines to read. Omit to use only the byte bound.
- `offset?: integer` — Text files only: 1-indexed line number to start reading from.

### Output

- Value — `string | object[]` — Bounded UTF-8 text or one image content item.
  - `type: "input_image"` (required)
  - `image_url: string` (required) — Prepared image data URL.
  - `detail: "high" | "original"` (required) — Requested image detail level.

## `write`

Create or atomically replace a UTF-8 file, creating parent directories as needed. Use for new files or intentional whole-file rewrites; use `edit` for targeted changes. Returns a short confirmation or plain error text.

### Input

- `path: string` (required) — Path to create or replace, relative to the working directory or absolute.
- `content: string` (required) — Complete file content.

### Output

- Value — `string`.

## `edit`

Atomically edit one UTF-8 file of at most 64 MiB with exact replacements. Every non-empty `oldText` must occur exactly once in the original file; replacements are matched independently, must not overlap, and either all apply or none do. Put multiple disjoint changes in one call and keep each `oldText` minimal but unique. Returns a short confirmation or plain error text.

### Input

- `path: string` (required) — Path to the UTF-8 text file, relative to the working directory or absolute.
- `edits: object[]` (required) — Non-overlapping replacements matched independently against the original content.
  - `oldText: string` (required) — Non-empty exact text that must occur once.
  - `newText: string` (required) — Replacement text.

### Output

- Value — `string`.

## Hosted web search

The Responses API can search and browse the live web using text and image results. URL citations in assistant output are displayed as clickable source links.
