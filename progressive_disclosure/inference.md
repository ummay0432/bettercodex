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
- Install compaction transactionally: validate the opaque output, restore exact
  turn-scoped context, prove the replacement restores automatic-compaction
  headroom, and persist it before advancing cache/window lineage.
- Exercise repeated compaction cadences and cold resume. A one-compaction test
  cannot prove that older summaries, tool artifacts, or selected skill bodies
  are replaced correctly over a long task.

## Evidence and validation

Keep public API contracts, upstream Codex implementation choices, and
bettercodex product decisions separate. One does not prove another. Use the
public OpenAI documentation for wire contracts, the Codex source linked from
`AGENTS.md` for upstream behavior, and this repository's source and tests for
what bettercodex actually does. Recheck live sources before porting behavior
that may have changed.

When model capabilities or prompt design are part of the task, fetch the live
official OpenAI model guidance rather than relying on a checked-in snapshot or
redirect file.

Changes to turns, Responses transport, history, compaction, saved sessions, or
tools need a test that drives the changed behavior and inspects the resulting
request, history, or tool output. Do not test only an extracted helper when the
behavior crosses those parts of the program.
