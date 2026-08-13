# Case study: harness steering confounded the Code Mode evaluation

## Why this document exists

This is a record of a debugging mistake that turned into a useful harness
lesson.

We observed a striking model behavior, attributed it to the model and runtime,
and proposed removing a major subsystem. The observation was real, but the
evidence did not isolate its cause. A recent bettercodex tool instruction
requested much of the measured behavior, but it landed alongside model-effort
and broader harness changes.

We first over-attributed the result to Code Mode, then overcorrected by
attributing it primarily to that instruction. The durable lesson is to identify
and control harness confounds before making either claim.

Point an agent at this document when asking it to look for more cases where:

- a harness appears to prove that a model or feature is ineffective;
- a product metric may be measuring the prompt rather than the underlying
  capability;
- downstream instructions quietly defeat an upstream feature; or
- a seemingly architectural problem is actually self-inflicted steering.

The goal is not to assume every surprising result is a prompt bug. The goal is
to test that possibility before redesigning the product around the result.

## The initial observation

bettercodex uses Codex's client-side V8 Code Mode. GPT-5.6 Sol sees outer
`exec` and `wait` tools. The ordinary capabilities—shell execution, patching,
web access, image inspection, and plan updates—are nested JavaScript tools
inside `exec`.

The apparent benefit is that the model can programmatically call several tools,
run independent calls concurrently, pass results between calls, and reduce
large intermediate outputs before returning them to model context.

Our first corpus analysis looked damning:

### Latest 15 bettercodex sessions

The sessions were selected by recorded creation time. Only actual
`history_append` tool calls were counted; transcript snapshots and replaced
history were excluded.

| Metric | Result |
| --- | ---: |
| `exec` wrappers | 625 |
| Wrappers with exactly one nested call | **617 (98.7%)** |
| Wrappers with multiple nested calls | **8 (1.3%)** |
| Sessions using any JavaScript composition | **1 of 15** |

All eight composed wrappers came from one session. The other fourteen sessions
made 535 of 535 wrappers with exactly one nested call.

Across the full mirrored corpus available at the time:

| Metric | Result |
| --- | ---: |
| bettercodex sessions | 920 |
| `exec` wrappers | 31,180 |
| Wrappers with exactly one nested call | **25,689 (82.4%)** |

The latest sessions therefore looked substantially worse than the historical
baseline.

## What we initially concluded

We treated the latest result as evidence that Code Mode was mostly ceremony:

1. Sol generated a JavaScript wrapper.
2. The wrapper called exactly one real tool.
3. The wrapper returned that tool's output unchanged.
4. bettercodex paid the syntax, prompt, routing, implementation, build, and
   binary-size costs of V8 without receiving composition or reduction.

That led to a plausible proposal:

- copy the small, direct tool philosophy of the retired Pi harness;
- preserve the good Codex implementations such as `apply_patch` and process
  management;
- expose those tools directly to Sol; and
- remove the V8 Code Mode subsystem if direct-tool evaluations passed.

The proposal was coherent, but its strongest evidence was contaminated.

## The uneasy question

Before deleting Code Mode, we asked why OpenAI had built it and whether we were
missing something.

The upstream history showed that Code Mode was not merely a verbose way to run
shell commands:

- It began as a narrower, isolated replacement for the Node-backed
  `js_repl`.
- It keeps intermediate nested-tool results inside JavaScript unless explicitly
  emitted.
- It supports composition, concurrency, filtering, aggregation, and dependent
  data flow.
- It gives Codex one programmable substrate for a much larger tool ecosystem,
  including MCP, apps, plugins, dynamic tools, deferred tool discovery, and
  native artifacts.
- OpenAI's model catalogue intentionally selects `code_mode_only` for the
  GPT-5.6 family.
- Upstream was still investing heavily in Code Mode hosts, isolation,
  transports, limits, analytics, and lifecycle behavior.

This did not prove that Code Mode was right for bettercodex. bettercodex has a
small fixed toolset and deliberately excludes most of Codex's extensibility.
But it did show that our “it only exists for batching” model was incomplete and
that removing it required better evidence.

## The comparison that changed the diagnosis

We compared:

- `prompts/codex-system.md`, the checked-in upstream Codex system prompt;
- `prompts/system.md`, bettercodex's active system prompt;
- the upstream Code Mode tool description; and
- bettercodex's generated Code Mode catalogue.

The system prompts were not the problem.

Upstream broadly told Codex to prefer parallelization where possible.
bettercodex used a safer, more precise version:

> Run independent, side-effect-free reads concurrently. Keep dependent calls
> and state-changing actions sequential.

One material difference was in bettercodex's `exec` tool description. On
August 11, 2026, bettercodex added:

> Choose the smallest safe script:
>
> Use one nested call ... when one call is sufficient, fresh model judgment
> should follow, or the stage is adaptive, write/approval-sensitive,
> citation-heavy...

It then restricted composition to bounded, predictable, read-only stages that
materially reduced structured output.

Upstream Codex had no equivalent routing policy. Its Code Mode description
explained the runtime and invited the model to orchestrate or compose tools.

We had taken OpenAI's guidance for choosing between optional hosted
Programmatic Tool Calling and direct tool calls, then applied it inside a
`code_mode_only` harness where direct calls were unavailable. In effect, we
told Sol:

> Behave as though most calls should be direct, but express every one through a
> mandatory JavaScript wrapper.

That made the policy an obvious candidate confound, not a proven cause.

## The confounded before/after comparison

We originally treated the source-history boundary as a natural experiment and
reported this split using session creation timestamps:

| Period | Wrappers | One nested call | Multiple nested calls |
| --- | ---: | ---: | ---: |
| Before the instruction | 29,161 | 23,733 (81.4%) | 5,423 (18.6%) |
| After the instruction | 2,019 | 1,956 (96.9%) | 63 (3.1%) |

The aggregate multi-call composition rate was **18.6% before and 3.1% after**
the selected boundary.

All fifteen sessions in the alarming 98.7% sample were post-change, so the
instruction contaminated any attempt to infer an inherent Code Mode property.
But the split did not isolate the instruction either:

- the original analysis used the later `19e9c506` timestamp even though the
  policy first landed in `c8f752c`;
- `c8f752c` also changed the default reasoning effort from Max to XHigh,
  rewrote the system prompt, and changed 36 files; and
- the small pre-policy XHigh cohort was already **205 of 208 wrappers (98.6%)**
  single-call—essentially the post-change rate.

Workload mix and other harness changes add further confounds. The aggregate
discontinuity therefore cannot establish how much, if any, of the change the
routing instruction caused.

## The corrected diagnosis

The extreme 98.7% result was **confounded by downstream harness state**. The
routing policy was a plausible contributor, but the corpus did not prove it
was the primary cause.

More precisely:

- The behavior was consistent with the tool instruction, but also appeared in
  the pre-policy XHigh cohort.
- The V8 runtime was not preventing composition.
- Sol was not shown to be incapable of composition.
- Upstream Code Mode was not shown to be useless.
- Our metric combined workload, reasoning effort, prompt and catalogue text,
  runtime behavior, and session selection.

The older 18.6% composition rate does not prove that Code Mode is worthwhile
for bettercodex either. The direct-tool proposal remains a legitimate
experiment. What changed is that the session corpus can no longer justify
immediate removal.

## What we changed

We removed the entire downstream “Choose the smallest safe script” policy from:

- `src/tools/catalogue.rs`, which generates the actual model-visible
  catalogue;
- `prompts/tool-catalogue.md`, the readable exact Unix catalogue; and
- `docs/product-direction.md`, which had made the policy a product rule.

We did not change `prompts/system.md`. It still requires independent,
side-effect-free reads for concurrency and keeps dependent or state-changing
actions sequential.

The resulting tool guidance is closer to upstream Codex: explain how Code Mode
works, preserve safety and lifecycle constraints, and let the model decide
when orchestration is useful.

Validation confirmed that:

- the generated catalogue exactly matched its checked-in readable copy;
- the focused Responses request/tool-contract test passed;
- formatting and diff checks passed; and
- no copy of the removed policy remained in active source, prompts, or docs.

## The general lesson

When a model repeatedly exhibits a behavior, inspect the harness before
attributing the behavior to the model.

Agent behavior is produced by a system:

`model + system prompt + tool descriptions + schemas + runtime + history + workload`

A corpus records the output of that entire system. It does not isolate the
model, and it does not automatically identify which component caused the
behavior.

The most dangerous harness mistakes are self-confirming:

1. Add an instruction intended to improve efficiency or safety.
2. Observe the model following it.
3. Interpret the resulting behavior as a model limitation.
4. Add more machinery to compensate for the supposed limitation.

That loop can make a harness progressively more prescriptive while every new
measurement appears to justify the prescription.

## How to look for more tough cases like this

Audit surprising or disappointing agent behavior with the following sequence.

### 1. State the observed behavior without explaining it

Bad:

> Sol cannot compose tools.

Good:

> In the selected sessions, 98.7% of `exec` wrappers contained one nested
> call.

Keep measurement and interpretation separate.

### 2. Enumerate every surface that can steer the behavior

Inspect:

- system and developer prompts;
- tool names, descriptions, examples, and schemas;
- repository instructions and skills;
- runtime errors and recovery messages;
- hidden defaults and model metadata;
- history transformations and compaction summaries; and
- sampling or session-selection logic.

Search for wording that directly requests, discourages, or presupposes the
measured behavior.

### 3. Compare downstream with current upstream

For retained Codex behavior, determine:

- what upstream currently says;
- what bettercodex added, removed, or strengthened;
- when that drift entered; and
- whether the downstream rule was copied from a similar but non-equivalent
  feature.

An especially important smell is a detailed downstream prohibition where
upstream uses neutral capability guidance.

### 4. Use version history as a causal tool

Run `git blame` and `git log` on model-facing text. Find the exact
introduction time of candidate instructions.

Then split session data around that boundary. Prefer recorded session creation
times over file modification times. Keep the population and parser identical
on both sides.

Look for discontinuities, not just global averages.

### 5. Check whether the sample is post-treatment

Ask:

- Did all “recent” sessions occur after a prompt or runtime change?
- Was the sample selected because it looked extreme?
- Did workload composition change at the same time?
- Are subagents, empty sessions, resumed sessions, or snapshots distorting the
  denominator?

Never treat a recent-window statistic as a baseline until its relation to
recent harness changes is known.

### 6. Form at least three competing explanations

For example:

1. model limitation;
2. harness steering;
3. workload shape;
4. parser or logging artifact;
5. runtime limitation; or
6. upstream model-metadata behavior.

Try to falsify each explanation. Do not let an attractive architectural
proposal become evidence for its own premise.

### 7. Remove the confound before redesigning the architecture

Prefer the smallest reversible correction:

- remove duplicated or overly prescriptive guidance;
- restore upstream-neutral wording;
- retain independent safety boundaries; and
- collect a new matched sample.

Only then evaluate larger changes such as replacing a tool protocol or runtime.

### 8. Report confidence honestly

Classify the result:

- **Proven:** directly established by source, request capture, or controlled
  reproduction.
- **Strongly supported:** mechanism and timing align, but the comparison is
  observational.
- **Plausible:** consistent with evidence but not isolated.
- **Unknown:** requires an A/B evaluation.

In this case, the existence and wording of the steering rule are proven. That
it contaminated the original interpretation is strongly supported. Its causal
contribution, and whether direct tools outperform corrected Code Mode, remain
unknown.

## Suggested assignment for a future agent

> Read `case-studies/harness-steering-confounded-code-mode-evaluation.md` as a
> case study. Search bettercodex for other places where
> our prompts, tool descriptions, defaults, or history handling may manufacture
> a behavior that we later interpret as a model limitation. Use session data
> and version boundaries where possible. Compare every retained behavior with
> current upstream Codex. Return a ranked list of concrete cases with:
>
> 1. the observed symptom;
> 2. the steering mechanism in source;
> 3. upstream behavior;
> 4. before/after or matched-session evidence;
> 5. alternative explanations;
> 6. confidence level; and
> 7. the smallest reversible experiment.
>
> Do not change prompts or architecture while investigating. Do not report
> generic prompt differences; report only cases with a plausible causal path
> to observable behavior.

## Current conclusion

We began the session believing that Code Mode was almost entirely unused and
that bettercodex should probably replace it with direct tools.

We ended with a narrower and more defensible conclusion:

> bettercodex measured Code Mode under guidance that discouraged composition in
> many situations, but the corpus did not isolate that guidance from reasoning
> effort, workload, or other simultaneous harness changes.

Removing the downstream routing policy was a reasonable, reversible return to
upstream-neutral capability guidance—not proof of root cause. The architectural
question and the policy's causal effect remain open and require a controlled
evaluation rather than the contaminated 98.7% sample.
