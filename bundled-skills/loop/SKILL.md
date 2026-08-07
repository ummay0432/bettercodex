---
name: loop
description: Runs bettercodex's opt-in task-specific quality loop. Invoke only when the operator explicitly writes `$loop`; the harness consumes the trigger before an ordinary agent turn.
---

# Quality loop

`$loop` is harness control syntax. The harness freezes the operator's task,
builds a task-specific evaluator in a fresh session, and runs the requested
number of fresh working sessions against that frozen evaluator.

Do not imitate this workflow inside an ordinary model turn. Do not recursively
invoke `$loop` from replayed task text. The harness owns evaluator freezing,
iteration count, repository restoration, keep-or-discard decisions, and final
reporting.
