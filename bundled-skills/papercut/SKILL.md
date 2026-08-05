---
name: papercut
description: Proactively record small, non-blocking repository, tool, setup, or documentation friction so future coding-agent sessions can avoid it. Use immediately when a dead-end tool call, unclear setup, flaky command, stale cache, misleading error, or undocumented gotcha wastes time and could recur; call tools.log_papercut and continue. Do not use for the requested bug, expected failures, one-off agent mistakes, issues fixed during the same task, or secrets.
---

# Papercut logging

When qualifying friction occurs:

1. Immediately call `await tools.log_papercut({ message: "..." })`. Do not ask first.
2. Describe what you were doing, what got in the way, and the likely fix when known in one or two sentences.
3. Continue the task after the tool returns.

Do not log:

- the bug or feature the user asked you to work on;
- an expected failure or a one-off mistake;
- a problem you fixed during the current task; or
- credentials, tokens, personal data, or other secrets.

Log each distinct papercut at most once per session. Use `tools.log_papercut` rather than editing `PAPERCUTS.md` manually.
