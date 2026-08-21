# Subagents

This directory contains the four fixed bettercodex `$deepwork` child specialist definitions described in [`SPEC-SUBAGENTS.md`](../SPEC-SUBAGENTS.md):

- `acceptance.md` — evidence-backed completion contract and verification surfaces;
- `manifest.md` — official documentation research and constrained handoff;
- `worker.md` — implementation;
- `reviewer.md` — final surgical review and refinement.

All four prompts are approved definitions ready for runtime integration.

Main's approved orchestrator prompt lives in the user-invoked `$deepwork` skill at [`bundled-skills/deepwork/SKILL.md`](../bundled-skills/deepwork/SKILL.md). It has no fixed child model profile.

`anthropic-code-reviewer-reference.md` is a non-production format reference adapted from Anthropic's official Claude Code subagent documentation. It demonstrates Claude Code's YAML-frontmatter-plus-Markdown-body convention. It is not a bettercodex specialist and must not be loaded into the `$deepwork` pipeline.

Source: https://code.claude.com/docs/en/sub-agents\
Retrieved: August 20, 2026
