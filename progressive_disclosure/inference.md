# Inference, history, transport, and tools

Read this before changing requests, Responses items, history, reasoning,
compaction, caching, streaming, recovery, tools, or saved sessions.

## Invariants

- Build model history incrementally; do not rewrite earlier history during
  normal turns. Keep stable request items stable so prompt caching can work.
- Put a hard bound on every new model-visible item and tool result. Review a new
  item carefully if it can exceed 1,000 tokens; no single item may exceed 10,000
  tokens.
- Search for breakage in CLI arguments, saved JSONL sessions and resume,
  Responses request and output items, and model-visible tool names and schemas.

## Evidence and validation

Keep public API contracts, upstream Codex implementation choices, and
BetterCodex product decisions separate. One does not prove another. Use the
public OpenAI documentation for wire contracts, the Codex source linked from
`AGENTS.md` for upstream behavior, and this repository's source and tests for
what BetterCodex actually does. Recheck live sources before porting behavior
that may have changed.

`docs/5-5-prompt-guidance.md` is a local copy of public GPT-5.6 guidance. Read
it only when model capabilities or prompt design are part of the task; verify
time-sensitive claims against the live OpenAI documentation.

Changes to turns, Responses transport, history, compaction, saved sessions, or
tools need a test that drives the changed behavior and inspects the resulting
request, history, or tool output. Do not test only an extracted helper when the
behavior crosses those parts of the program.
