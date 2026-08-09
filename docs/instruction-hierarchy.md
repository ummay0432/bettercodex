# Instruction hierarchy and untrusted context

Read this before changing message roles, repository or skill injection, tool
results, compaction, or prompt-injection handling.

## Contract

OpenAI's instruction authority is expressed by API roles, not by filenames,
XML-like labels, headings, or prose that claims priority. Higher-authority
instructions override conflicting lower-authority instructions; untrusted data
does not gain authority merely because the model can read it. Recheck the live
[Model Spec](https://model-spec.openai.com/) before changing these assumptions.

Instruction hierarchy reduces model misbehavior but is not a sandbox.
bettercodex runs commands with the invoking user's permissions, so role
separation must never be presented as a security boundary.

## Current role map

Source is authoritative; verify these paths before relying on this summary.

- `src/api.rs` renders `prompts/system.md` with one target-specific platform
  fragment and sends it through the Responses API's top-level `instructions`
  field. It sends the typed tool catalogue as a developer item.
- `src/context.rs` sends bounded `AGENTS.md` repository context as a user item
  and environment context as a developer item.
- `src/skills.rs` sends bounded skill metadata and selected `SKILL.md` bodies as
  user items. Harness-owned skill policy remains in `prompts/system.md`.
- Tool calls and results remain native Responses items.

## Change rules

1. Preserve each source's real API role and item kind. Do not flatten messages
   or promote lower-authority content for convenience.
2. Keep harness rules and tool definitions at developer authority and operator
   requests at user authority.
3. Keep repository instructions below the harness contract. Loading a relevant
   file delegates only its applicable instructions, not unrelated text in the
   repository.
4. Keep skill-framework policy separate from skill metadata and bodies. A
   repository- or user-authored skill must not become developer instructions by
   string interpolation.
5. Keep command output, file contents, web results, and other external data in
   native tool outputs or clearly delimited data fields. Escape any delimiter
   used by a wrapper.
6. Preserve provenance through incremental requests, saved sessions, resume,
   interruption recovery, and compaction. A summary must not promote ignored
   tool or repository text.
7. Do not add repeated prompt-injection warnings, prompt sandwiches, monitors,
   or output rewriting without evidence that the change improves real tasks
   without causing refusal or capability regressions.
8. Before changing a boundary, inspect the current public API contract and the
   corresponding current upstream Codex implementation. Neither substitutes
   for inspecting bettercodex itself.
9. Test the final request or replay path affected by a structural change. Add a
   behavioral evaluation only when the change can alter model behavior and a
   deterministic repository test cannot establish the result.
