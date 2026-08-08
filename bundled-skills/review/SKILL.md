---
name: review
description: "Proactively review, repair, and refactor a user-specified part of a software system during implementation work. Invoke whenever a thorough engineering audit could improve correctness, simplicity, performance, resource efficiency, or maintainability; `$review` or `/review` is not required. Do not use for read-only requests."
---

# Review

Thoroughly review the target specified by the user. Establish a deep
understanding of the affected product before proceeding.

Ask yourself: Would an exceptional senior engineer—a proactive perfectionist
expecting to own this system for years—be satisfied with its current state? Look
for the following success criteria:

- Could it be simpler, with a smaller footprint?
- Could it be more responsive and performant?
- Could it be more resource-efficient?
- Could it be easier to understand, extend, and maintain?

Conduct rigorous web research into current authoritative documentation to
uncover improvements to the target beyond what your built-in knowledge alone
would surface. Use what you find to re-evaluate every success criterion; do not
let your knowledge cutoff or initial assumptions limit the review.

If any answer is yes and repository evidence supports a clear net improvement,
take action. Refactor the affected system as deeply as needed. Remove everything
the improved design makes obsolete.

Implementation is complete only when all success criteria are satisfied.

Do not invent defects, contrived edge cases, or artificial validation work to
force a change; if the affected system already meets these standards, stop
there, leave it unchanged, and explain the evidence supporting that conclusion.
