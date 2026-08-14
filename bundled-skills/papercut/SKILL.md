---
name: papercut
description: Proactively record small, non-blocking repository, tool, setup, or documentation friction so future coding-agent sessions can avoid it. Use immediately when a dead-end tool call, unclear setup, flaky command, stale cache, misleading error, or undocumented gotcha wastes time and could recur; append it to PAPERCUTS.md with the direct file tools and continue. Do not use for the requested bug, expected failures, one-off agent mistakes, issues fixed during the same task, or secrets.
---

# Papercut logging

When qualifying friction occurs:

1. Immediately use `read` on the repository-root `PAPERCUTS.md`. Do not ask first.
2. Describe what you were doing, what got in the way, and the likely fix when known in one or two sentences.
3. Use `edit` with one exact, unique replacement near the end of the file to append a `- ` list item. If the read established that the file does not exist, use `write` to create it with a `# Papercuts` heading and the item; never overwrite an existing file with `write`.
4. Continue the task after the edit returns.

Do not log:

- the bug or feature the user asked you to work on;
- an expected failure or a one-off mistake;
- a problem you fixed during the current task; or
- credentials, tokens, personal data, or other secrets.

Log each distinct papercut at most once per session.
