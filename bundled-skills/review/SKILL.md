---
name: review
description: "Thoroughly review, repair, and refactor a software target. Use only when the user explicitly invokes `$review` or `/review`; never select this skill proactively. Do not use for read-only requests."
---

# Review

Use two phases so the full review begins with a fresh, concrete target.

## Select the target

If the user already specified a concrete target, treat it as selected.
Otherwise, stay in target-selection mode and inspect read-only evidence until
you can isolate one bounded product, component, or behavior whose review could
produce a meaningful improvement. Do not modify files or load the full review
protocol while selecting the target.

State the selected target and the evidence that made it worth reviewing.

## Review the target

Immediately before reviewing or changing the selected target, read
[`references/review-protocol.md`](references/review-protocol.md) completely and
follow it. If context is compacted while the review remains active, read the
protocol again before continuing.
