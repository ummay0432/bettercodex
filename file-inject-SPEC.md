# File Injection Specification

## Initial prompt (verbatim)

```text
claude code has this really nice feature where when in the composer, writing to the agent, any mentioned file tagged by @ for example @SPEC-SUBAGENTS.md, it makes it so that the file is just injectec into the context of the agent, rather than the agent having to waste a tool call to read the file.

Question is, is there a reason upstream codex isnt doing this? If yes, why? Figure it out.

if its deemed that its actually better to adopt that claude code feature, let me know.

Dont make changes
```

## Research output

## Verdict

**Yes, bettercodex should adopt this feature—but as a bounded, structured file attachment, not by blindly expanding arbitrary files into raw prompt text.**

## What upstream Codex currently does

I checked upstream `main` on **August 20, 2026**, at commit `4a94288`.

Selecting a file through `@`:

1. Replaces the active `@token` with the file path.
2. Submits that path as ordinary `UserInput::Text`.
3. Does not create a local text-file attachment or read the file.

The protocol supports structured images, skills, and other mentions, but no equivalent structured text-file input. So the model sees `SPEC-SUBAGENTS.md` and normally has to call a tool to read it.

## Why upstream does not do it

The historical answer is surprising: **Codex used to support exactly this.**

- Issue #700 requested that `@file` add the file directly to context rather than relying on shell commands. 【0†source】([github.com](https://github.com/openai/codex/issues/700))
- On **April 30, 2025**, PR #701 implemented it in the old TypeScript CLI. Immediately before model submission, `@path` was replaced with an XML block containing the complete file contents; the UI continued displaying the compact `@path`. 【0†source】([github.com](https://github.com/openai/codex/commit/bc4e6db7496ba3265e79e0f2ffb55b197d646f8a))
- During the Rust transition, PR #1401 added full-repository fuzzy `@` file search, but only implemented inserting the selected path into the composer. It did not port the content-expansion half. 【0†source】([github.com](https://github.com/openai/codex/pull/1401))
- On **August 8, 2025**, the remaining TypeScript CLI was deleted, taking the old implementation with it. 【0†source】([github.com](https://github.com/openai/codex/pull/2048))
- A later request to restore full-file inclusion, issue #3413, was closed as “not planned” on **December 14, 2025** because it had received no upvotes or further discussion—not because of a stated technical objection.

**Therefore, the best-supported conclusion is that it was lost during the TypeScript-to-Rust migration and subsequently deprioritized.** I found no maintainer statement saying eager file injection was fundamentally undesirable, insecure, or incompatible with Codex.

## There are legitimate tradeoffs

They do not explain the historical omission, but they explain why the naive implementation is imperfect:

- It eliminates a model/tool round trip, but **not the tokens needed to send the file contents**.
- Large or repeated mentions can consume substantial context and trigger earlier compaction.
- Binary, non-UTF-8, unreadable, symlinked, or changing files need defined behavior.
- The captured contents become a snapshot that can become stale.
- Composer-side reads need to honor the same access controls as tool-based reads.
- File contents must retain user-data provenance rather than accidentally becoming system or developer instructions.

Claude Code explicitly confirms that `@file` includes the entire file without a tool call. Consequently, its `Read` PreToolUse hooks do not fire; Anthropic instead applies `Read` deny rules to prevent forbidden files from being included.

## Recommendation for bettercodex

**Adopt it.** An explicitly selected `@file` is strong user intent, and making the agent spend another inference round trip discovering and reading that same file is unnecessary latency and a potential reliability failure.

I would define it this way:

- Keep the visible `@path`/path marker in the composer and transcript.
- Bind a successfully selected file as a structured attachment.
- Read it once when submitting and send the captured contents as a separate, clearly delimited **user-context item**.
- Support UTF-8 text files only.
- Enforce per-file and per-turn context limits.
- Never silently truncate: warn and fall back to a path-only reference when oversized.
- Deduplicate repeated mentions within a turn.
- Apply normal read restrictions and deny rules.
- Treat directories as listings or path references, not recursive content dumps.
- Preserve the current special handling for images.

There is already upstream precedent for the correct architecture: explicit skill mentions create a structured `UserInput::Skill`, after which core injects the matching full `SKILL.md`. During review, maintainers specifically preferred an explicit input item similar to image attachments rather than reparsing raw prompt text. 【0†source】([github.com](https://github.com/openai/codex/pull/7763))

So: **Claude Code has the better default behavior here. bettercodex should adopt the capability, but with modern structured provenance and context limits rather than copying Codex’s old unrestricted XML expansion.**

No files were changed.
