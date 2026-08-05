# Model-facing context

Read this before changing `AGENTS.md`, `prompts/*.md`, tool descriptions, or
model-visible errors.

## Boundaries

Do not alter Codex-derived agent-facing context in `prompts/*.md`, tool
descriptions, or model-visible errors without explicit user permission.

`prompts/system.md` is the active system prompt. Edit it only when the user
explicitly asks to edit the system prompt. `preserve/system-prompt.md` shows the
rough direction behind it, not a finished specification.

`prompts/tool-catalogue.md` contains the exact generated tool text.
`prompts/tool-context.md` records the complete tool-related request prefix and
its context cost; update its generated regions through
`./scripts/dev.py tool-context --update`.

## Writing and placement

Before editing `AGENTS.md`, read `docs/writing-a-good-claude-md.md` completely.
Keep `AGENTS.md` to facts and instructions useful in almost every BetterCodex
session. Put task-specific context in `progressive_disclosure/`, implementation
contracts with the code that owns them, and historical material under
`preserve/`.

When moving or restructuring existing prose, also read and apply
`docs/writing-instructions.md`: preserve the author's voice and intent, and
separate descriptive rationale from operational rules.

In `AGENTS.md`, system prompts, tool descriptions, and model-visible errors,
name the actual file, source, and required action. Do not hide them behind
phrases such as “general behavior,” “project context,” “take precedence,” or
“handle appropriately.”
