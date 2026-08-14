# Model-facing context

Read this before changing `AGENTS.md`, `prompts/*.md`, tool descriptions, or
model-visible errors.

## Boundaries

Do not alter Codex-derived agent-facing context in `prompts/*.md`, tool
descriptions, or model-visible errors without explicit user permission.

`prompts/system.md` is the active harness template. `src/api.rs` sends it in the
normal Responses request's top-level `instructions` field alongside the
fixed top-level tool catalogue. It has developer authority; it is not OpenAI's
root or system layer. Edit it only when the user explicitly asks to edit the
system prompt.

`src/tools.rs` generates the fixed catalogue of four ordinary functions plus
hosted `web_search`. `prompts/tool-catalogue.md` records the exact generated
catalogue in readable form. Refresh it from `bcodex --tool-catalogue` whenever
the generator changes.

## Writing and placement

Keep `AGENTS.md` to facts and instructions useful in almost every bettercodex
session. Put task-specific context in the linked documents under `docs/`,
implementation contracts with the code that owns them, and historical material
outside active instructions.

When moving or restructuring prose, preserve the author's voice and intent and
keep descriptive rationale separate from operational rules.

In `AGENTS.md`, system prompts, tool descriptions, and model-visible errors,
name the actual file, source, and required action. Do not hide them behind
phrases such as “general behavior,” “project context,” “take precedence,” or
“handle appropriately.”
