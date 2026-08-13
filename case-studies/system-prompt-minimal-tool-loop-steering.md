# Case study: system-level minimal-loop steering may suppress proactive work

## Status

The steering instruction and its drift from current upstream Codex are proven.
The instruction was removed on August 13, 2026. Its effect on task quality is
plausible but has not been isolated. This document records the risk and the
smallest controlled experiment; it does not claim a measured regression.

## Why this document exists

The [Code Mode case study](harness-steering-confounded-code-mode-evaluation.md)
found a downstream instruction that discouraged the behavior later used to
judge Code Mode. A similar pattern was found one level above the tool catalogue,
in bettercodex's active system prompt.

The system prompt said:

> Resolve requests in the fewest useful tool loops without sacrificing
> correctness, required evidence, or validation. After each result, stop if the
> request can be completed; otherwise take the smallest useful next step.

This is not a safety boundary or an outcome requirement. It makes tool loops,
stopping opportunities, and step size explicit optimization targets for every
task.

## Why the instruction is risky

The qualification about correctness, evidence, and validation is useful, but
the operative instructions still repeatedly favor less work:

- use the fewest loops;
- test for a stopping point after every result; and
- choose the smallest next step.

That can steer the model away from broader discovery, adjacent defect repair,
cross-checking, and validation whose necessity becomes visible only after more
of the system is inspected. It can also create a self-confirming metric: fewer
calls or loops may look like improved efficiency even when the instruction
itself produced them.

The causal path is direct, but degradation is not established. On a narrow task,
the instruction may reduce redundant work without affecting the result. Lower
tool use can reflect workload shape or better model judgment rather than
premature stopping.

## Source history and confounds

The instruction entered in `c8f752c` on August 11, 2026, as part of the same
36-file GPT-5.6 specialization that changed the system prompt, reasoning effort,
model metadata, and tool runtime. The Code Mode routing policy also entered at
that boundary.

Consequently, sessions before and after `c8f752c` do not isolate this sentence.
Tool-call counts alone cannot establish its effect, and the contaminated Code
Mode composition corpus cannot be reused as a quality evaluation.

Reasoning effort is outside this case. Any evaluation must hold the bettercodex
default of XHigh fixed so effort does not become another confound.

## Comparison with current upstream Codex

The relevant current upstream prompt is preserved in
[`prompts/codex-system.md`](../prompts/codex-system.md). It does not instruct the
model to minimize tool loops, repeatedly test for an early stop, or take the
smallest next step. Its tool-routing guidance instead says:

> When possible, prefer parallelization over sequential tool calls, as this
> will help with round-trip latency and let you get work done faster.

bettercodex already retains a stricter, safety-aware version of that behavior:

> Run independent, side-effect-free reads concurrently. Keep dependent calls
> and state-changing actions sequential, and synthesize retrieved results before
> acting.

The generic minimal-loop instruction was therefore downstream steering, not a
retained upstream requirement.

## GPT-5.6 prompting guidance

The checked-in [GPT-5.6 guidance](../docs/5.6-prompting.md) cuts against keeping
the instruction:

- GPT-5.6 can infer the intended level of work, so prompts often do not need to
  prescribe every step.
- Prompts should state the goal, context, constraints, evidence, success
  criteria, and output format.
- Leaner prompts should be produced by removing one instruction group at a time
  and rerunning the same representative evaluations.
- Fewer calls, turns, or intermediate outputs count as improvements only after
  the final result still meets the quality bar.

The minimal-loop sentence prescribes a global process heuristic and promotes a
resource metric into an objective before task quality has been evaluated.

## Competing explanations

At least four explanations must remain live during evaluation:

1. The instruction causes premature stopping or shallow discovery.
2. The instruction harmlessly removes redundant loops.
3. GPT-5.6 independently chooses fewer calls because it is more efficient.
4. Differences come from workload shape, the removed Code Mode policy, or
   another simultaneous harness change.

Only a matched evaluation can separate them.

## Applied upstream-faithful correction

The minimal-loop sentence was deleted from `prompts/system.md` without being
replaced.

That is preferable to adding an inverse instruction such as “use more tools” or
another detailed routing policy:

- it restores upstream-neutral model judgment;
- it follows GPT-5.6's lean-prompt guidance;
- the existing concurrency rule still governs safe parallel work;
- the autonomy and persistence sections still require completion; and
- task-specific repository instructions can still require particular evidence
  or validation where needed.

No model default, tool schema, runtime behavior, or other prompt text changed
with the correction.

## Controlled evaluation

Compare the archived baseline containing the minimal-loop sentence with the
corrected prompt that removes only that sentence. Keep the model, XHigh effort,
tool catalogue, repository revision, task inputs, and scoring rubric identical.

Use representative cases that include:

- a repository-wide audit where adjacent findings matter;
- multi-stage debugging where early evidence can support several hypotheses;
- an implementation requiring discovery, repair, and validation; and
- a narrow routine task that can expose unnecessary work in the candidate.

Score task success, correctness, completeness, missed material findings,
required evidence, and introduced defects before examining calls, loops, tokens,
or latency. Review outputs blind to prompt variant. Treat lower resource use as
an improvement only among outputs that pass the same quality bar.

## Confidence

- **Proven:** the instruction existed, globally favored minimal loops and steps,
  entered with `c8f752c`, and had no equivalent in current upstream Codex.
- **Plausible:** it can suppress proactive discovery or validation and can
  manufacture low-loop behavior.
- **Unknown:** its net effect on bettercodex task quality and efficiency.

## Current conclusion

The removed sentence was an unnecessary global routing policy with a credible
path to the same self-confirming harness behavior documented in the Code Mode
case. The right next step is a matched A/B evaluation—not attributing existing
session behavior to the sentence and not compensating with a new prescriptive
rule.
