# Case study: DeepSeek's composability paper points to phase-aware tool recovery, not a plugin-system port

## Status

This document records an August 17, 2026 investigation of *A Programming
Paradigm for Spatiotemporal Composability* by Yifan Shi, Wei Zhang, and Tianyi
Cui. The paper presents Cordis, the component model used by DeepSeek Harness,
and explicitly identifies self-evolving agent harnesses as a target domain.

The investigation produced one primary recommendation:

> Evaluate an internal, effect-aware tool lifecycle journal so cold resume can
> distinguish a tool that never started, a file whose exact intended post-state
> is now present, a file that still matches its pre-state, and an opaque command
> whose outcome is unknown.

This is an evaluation proposal, not a production decision. No runtime, prompt,
tool schema, model-visible error, session format, or product behavior changed as
part of this case study.

The source snapshot preserved beside this document is:

- [`deepseek-spatiotemporal-composability-2026-08-13.pdf`](deepseek-spatiotemporal-composability-2026-08-13.pdf)
- upstream repository revision:
  [`948a07b`](https://github.com/cordiverse/paper/tree/948a07b369c62adb3b12e102458be5c18dfb69b9)
- stated draft date: August 13, 2026
- retrieved: August 17, 2026
- size: 2,140,840 bytes
- SHA-256:
  `4d48478dc0b6222d9f74d7db10ee776449b1209eb112632336544d32a49db97f`

The paper repository calls the PDF an active revision. Keeping a dated local
snapshot is therefore important: future revisions may change the claims or
examples that motivated this analysis.

Adjacent implementation documentation was inspected at DeepSeek Harness
revision
[`47f9438`](https://github.com/deepseek-ai/DeepSeek-Harness/tree/47f943859bef60e4160492346772ded9b24f765a),
including its
[architecture](https://github.com/deepseek-ai/DeepSeek-Harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/architecture.md),
[Cordis primer](https://github.com/deepseek-ai/DeepSeek-Harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/cordis-primer.md),
and
[lifecycle tutorial](https://github.com/deepseek-ai/DeepSeek-Harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/cordis-tutorial/04-lifecycle.md).
Those documents corroborate the implementation model but do not substitute for
the paper's formal claims or limitations.

## Executive verdict

**Port the invariants, not Cordis.**

The paper contains useful harness-engineering principles:

1. every runtime acquisition should have an owner and a teardown path;
2. lifecycle phase should be explicit rather than inferred from missing output;
3. desired state should be reconciled incrementally against live state;
4. a provider should stop accepting new dependents before existing dependents
   drain and the provider is destroyed; and
5. irreversible emissions must be treated differently from reversible internal
   effects.

bettercodex already implements much of this in a smaller, Rust-native form:
RAII process cleanup, temporary-file cleanup, atomic file replacement,
transactional compaction, bounded context, and declarative world-state refresh.
A Cordis runtime, plugin graph, dynamic tool loader, hot-module-reload system, or
configuration framework would duplicate existing mechanisms while violating
bettercodex's fixed product scope.

The material gap is narrower. bettercodex durably records turns, model history,
and completed tool outputs, but tool execution start and mutation completion are
not durable lifecycle phases. If the process dies after `write`, `edit`, or `bash`
has acted but before its output is appended, resume can only synthesize
`aborted` and tell the model that the action may have partially executed.

The best paper-derived candidate is therefore **phase-aware cold-resume
recovery for tool calls**. It is principally a correctness and reliability
improvement. It should be nearly neutral for model context and tokens during
successful turns, slightly negative for local journal I/O, and beneficial for
tool calls and tokens only after an interruption by avoiding blind inspection
and duplicate execution.

A second, lower-confidence idea—content-addressed skill definitions with small
turn-scoped activation references—could target steady-state context cost. The
paper motivates separating provider identity from activation, but it provides
no evidence that this encoding improves an LLM harness. Prompt caching,
compaction, and model adherence make that a separate evaluation rather than a
recommendation to ship.

## Why this paper required surgical reading

The attractive headline interpretation is:

> DeepSeek built an everything-is-a-plugin harness, so bettercodex should become
> dynamically extensible too.

That is not what the evidence establishes.

The paper's central problem is safe composition in long-lived systems whose
components appear, disappear, fail, and change dependencies at runtime. Its
formal contribution is a semantics for those transitions. Cordis is one
implementation, and Koishi is an observational case study showing that the
model can support a large TypeScript plugin ecosystem.

The paper does **not** establish that:

- plugin systems make coding agents more capable;
- dynamic tools reduce context, tool calls, tokens, or latency;
- self-modifying harnesses are safer than fixed harnesses;
- arbitrary shell effects can be rolled back;
- declared dependencies form a security sandbox; or
- Cordis is a better implementation strategy for a fixed four-tool Rust
  binary.

The right question is not whether Cordis is impressive. It is:

> Which guarantees solve a real bettercodex failure mode more cheaply than the
> mechanisms bettercodex already has?

## The paper's model

### Temporal composability: mutations carry their own inverse

A component installation changes a shared context. In the paper's model, each
such effect is paired with a disposer that reverses it. Effects compose in
registration order; disposal unwinds them in reverse order. If startup fails
partway through, only the successfully accumulated prefix is unwound.

This addresses a familiar failure pattern in plugin systems: startup and
shutdown are implemented as distant, manually synchronized code paths, so a
new acquisition is added without updating every failure and unload path.
Co-locating acquisition and cleanup makes the lifecycle obligation local.

The guarantee has a hard boundary. It works only when:

- the mutation passes through the tracked context;
- the runtime owns the affected resource;
- the inverse remains valid when disposal runs; and
- external observers or concurrent actors have not made reversal unsafe.

Deleting a listener, cancelling a timer, removing a service binding, or closing
a runtime-owned process is naturally reversible. Sending an email, publishing
a package, charging an account, or running arbitrary Bash is not. The paper
therefore distinguishes internal reversible acquisitions from emissions across
a system boundary, where withholding, explicit commit, idempotency, or
compensation is required instead of fictional rollback.

### Spatial composability: dependencies are reactive coeffects

A component also declares what it requires from the context. The runtime
observes whether those requirements are currently satisfied and activates or
deactivates the component as providers appear, disappear, or change identity.

This replaces scattered imperative checks with a declarative relation:

- effects describe what a component contributes;
- coeffects describe what it requires; and
- the runtime reconciles component state when the shared context changes.

The important detail is provider identity, not just a Boolean capability name.
If the provider instance changes, dependents may need to stop and restart even
when a service with the same name remains present.

The paper does not treat this as access control. A declaration lets the runtime
reason about activation; it does not prevent undeclared code from accessing
ambient process capabilities. That distinction matters for bettercodex, which
runs with the invoking user's permissions and deliberately does not claim a
sandbox.

### Fibers make lifecycle phase explicit

Cordis represents each component as a fiber with an explicit lifecycle rather
than a callback that is either vaguely "loaded" or "not loaded." Loading,
active service, dependency loss, unloading, disposal, and failure are distinct
states and transitions.

Three details are especially portable:

1. **Partial startup is still state.** If the third acquisition fails, the
   first two must be known and disposed.
2. **Transitions are inertial.** A load or unload transition is allowed to
   reach a safe boundary rather than overlapping incompatible transitions.
3. **Withdrawal and destruction are separate.** A provider can stop admitting
   new dependents while remaining usable long enough for current dependents to
   quiesce and clean up; only then is the provider destroyed.

The third principle prevents teardown-order bugs. Removing a provider from the
dependency graph and destroying the underlying object cannot be treated as one
instantaneous operation if dependent cleanup still needs it.

### Declarative loading is desired-state reconciliation

The component loader interprets configuration as desired state and incrementally
reconciles the live component tree toward it. Stable identity determines
whether a component is retained, reconfigured, replaced, loaded, or unloaded.
Transactional hot replacement protects the prior module/cache state when a new
version fails.

The valuable abstraction is not hot module replacement itself. It is:

> Compute a desired state, make the smallest valid transition toward it, and
> install the new state only after the transition is known to be sound.

bettercodex already applies this pattern to compaction and generated world
state. Adding a general component loader would not improve those mechanisms.

### Failure containment is local, not magical

A failed fiber rolls back the effects it accumulated and can remain quarantined
while unrelated siblings continue. This is stronger than crashing the whole
component tree, but it does not make failures disappear. A dependency failure
still propagates to dependents, and irreversible external emissions remain
outside rollback.

For an agent harness, the analogous goal is not "undo everything the model did."
It is:

- contain harness-owned resource leaks;
- preserve completed evidence;
- identify the lifecycle phase that failed;
- prevent blind re-execution; and
- continue from a truthful state.

## Mechanism-by-mechanism verdict

| Paper mechanism | Real guarantee | Important condition or limitation | bettercodex verdict |
| --- | --- | --- | --- |
| Revertible effects | Tracked acquisitions can unwind after unload or partial failure | The runtime must own the effect and a valid inverse must exist | Keep using Rust ownership and RAII; do not add a generic effect framework |
| Reverse-order disposal | Dependents/resources unwind in a safe structural order | Async teardown still needs explicit phase boundaries | Preserve for process groups, temporary files, and any future owned resource |
| Reactive coeffects | Components activate only while declared provider identities satisfy requirements | Dependency declaration is not sandboxing | No dynamic tool/component graph; current fixed dependencies do not need it |
| Fiber lifecycle | Loading, active, unloading, failed, and disposed are explicit | Every relevant transition must pass through the runtime | Port narrowly to durable tool execution phases |
| Desired-state reconciliation | Live state converges incrementally toward a declarative target | Stable identity and transactional installation are required | Already present in world-state refresh and compaction |
| Transactional hot replacement | Failed replacement can preserve the previous live component | Valuable only when runtime replacement is a product requirement | Reject; bettercodex has no plugin or HMR requirement |
| Failure quarantine | One failed component need not destroy unrelated siblings | Dependents still react to provider loss | Ordinary tool errors already become results; improve only cold-crash evidence |
| Koishi case study | Demonstrates feasibility in one mature TypeScript ecosystem | Observational, ecosystem-specific, and not a controlled harness benchmark | Treat as plausibility evidence, not an adoption result |

## Evidence strength and limitations

The paper combines formal semantics, a TypeScript implementation, and an
observational Koishi ecosystem case study. That is meaningful evidence that the
abstractions can be implemented and used at scale.

It is not controlled evidence about coding-agent outcomes. The paper does not
compare agent task success, recovery quality, developer productivity, token
cost, latency, or runtime overhead against simpler harness designs. Its own
agent-harness application is a motivating and future validation domain rather
than a completed bettercodex-like A/B.

The formal results are also conditional. They establish properties of the
modeled context transformations and lifecycle rules, not of arbitrary operating
system state or third-party services. A system receives the theorem's benefit
only to the extent that real effects are routed through the modeled boundary.

The practical conclusion should therefore remain narrow:

> The paper gives bettercodex a better vocabulary and lifecycle discipline for
> recovery. It does not give bettercodex evidence to adopt DeepSeek Harness's
> architecture.

## What bettercodex already gets right

### Harness-owned resources have concrete owners

`src/process_runtime.rs` wraps each Bash process group in `ProcessGuard`. Normal
completion, timeout, cancellation, and ordinary unwinding terminate the process
tree. `src/tools.rs` similarly owns atomic-write temporary files through a guard
that removes an uncommitted temporary path.

This is the Rust-native form of revertible effects. The acquisition and cleanup
obligation live together, and `Drop` covers error exits without requiring a
dynamic effect registry.

The boundary must remain explicit: a hard process kill cannot run `Drop`, and
terminating a process does not reverse filesystem, network, package-manager, or
service effects the process already emitted.

### File mutations are atomic at the target

`write` and `edit` prepare their result before entering the mutation boundary.
Once mutation starts, cancellation no longer reports a failure after a completed
replacement. Both use an atomic temporary-file replacement for the target.
`edit` additionally validates all exact replacements against the original file
and rejects the complete call if one is missing, duplicated, or overlapping.

This already prevents a large class of partial-file states. It does not prove
whether a completed atomic replacement happened if the process dies before the
tool result is journaled.

`write` has one additional boundary: it may create missing parent directories
before replacing the target. A crash can therefore leave directory acquisition
even when the intended file was not committed. Any recovery design must report
or record that auxiliary effect rather than claiming the whole operation was
reversible.

### Compaction is transactional

`src/context.rs` validates the compacted replacement, restores required world
and active-turn context, proves that the replacement restores headroom, and
persists it before advancing in-memory history lineage. `src/agent.rs` abandons
the transport baseline if a completed compaction response cannot be installed.

This closely matches the paper's desired-state rule: prepare and validate the
replacement, install it atomically at the harness boundary, then advance the
live state.

### Generated context is reconciled, not endlessly appended

`WorldState` computes the current environment, repository instructions, and
available-skills catalogue. Refresh removes stale generated items and inserts
the current set at the correct instruction-hierarchy position. If the saved
state is already current and correctly placed, it performs no history rewrite.

That is already a focused spatial-reconciliation mechanism. bettercodex does
not need a general reactive service graph to refresh three known context
providers.

### Session storage already handles journal-tail failure

`src/rollout.rs` writes append-only JSONL records, restores the prior boundary
if a record write fails, and repairs an interrupted final record on load. It
records turn start and turn finish separately.

The journal deliberately targets process-crash recovery using complete records
in the filesystem cache; it does not force every record through durable storage
for power-loss recovery. A tool-lifecycle extension should preserve that stated
boundary unless the product separately chooses a stronger durability contract.

## The actual gap: execution phase disappears between model history and tool output

The current execution order is approximately:

1. a completed model tool-call item is appended to conversation history;
2. `AgentEvent::ToolStarted` is emitted to the live UI;
3. the tool executes and may mutate the workspace or external systems;
4. `AgentEvent::ToolCompleted` is emitted to the live UI; and
5. after the selected calls finish, their function-call outputs are appended to
   durable conversation history.

The UI events are not durable lifecycle records. A process exit between steps
3 and 5 leaves a persisted tool call with no persisted output.

On cold resume, `src/rollout.rs` finds missing call-output pairs and normalizes
them with the synthetic output `aborted`. `src/context.rs` adds generic guidance
that any running command or tool may have partially executed and tells the model
to inspect before repeating it.

That behavior is conservative and protocol-correct, but `aborted` is not an
execution fact. It conflates at least five states:

1. the call was recorded but execution never started;
2. execution started but reached no intentional mutation;
3. the exact intended file state is present but no result attributes it to the
   tool;
4. the exact pre-state remains, although auxiliary directory creation may have
   occurred; and
5. Bash emitted an unknown subset of local or external effects.

Current upstream Codex uses the same synthetic `aborted` normalization for a
missing output at inspected revision
[`c6058cc`](https://github.com/openai/codex/blob/c6058ccaa91ab17159cf805bf4d6d4edd87fe5fc/codex-rs/core/src/context_manager/normalize.rs).
Consequently, a stronger implementation would be a deliberate bettercodex
recovery improvement unless upstream adopts an equivalent first.

This is the closest bettercodex analogue to the paper's motivating defect:
there is a real lifecycle transition, but the durable state does not represent
it. Recovery must infer too much from absence.

## Primary proposal: effect-aware tool recovery

### Objective

Evaluate whether a small internal lifecycle journal can make cold resume
truthful and deterministic without changing the successful-turn model context,
tool catalogue, or fixed four-tool product surface.

This should not be implemented as Cordis, a plugin API, or a general-purpose
effect system. It should be a closed bettercodex enum and a small set of session
records tied to the existing tools.

### Fixed effect classes

| Effect class | Current tool | Recovery contract |
| --- | --- | --- |
| Read-only observation | `read` | A missing result never means an intentional workspace mutation; the read result itself may be unavailable |
| Atomic local mutation | `write`, `edit` | Persist enough pre-mutation evidence to determine whether the intended target reached the exact post-state |
| Opaque emission | `bash` | Record whether execution started and durably finished; otherwise report the outcome as unknown and never imply rollback |

Hosted web search is outside the local function runtime and does not mutate the
workspace. Its interrupted-response handling should remain governed by the
Responses transport unless a separate failure is demonstrated.

### Minimum durable phases

The exact record schema must be designed against current upstream Codex before
implementation, but the semantics should include:

1. **Started**
   - persisted before invoking a mutating or opaque tool whose execution status
     matters;
   - identifies the call and fixed effect class;
   - distinguishes a recorded call that never began from one that did; and
   - may be omitted for `read`, whose missing result has no intentional
     workspace effect, if measurement shows no recovery value.

2. **Prepared mutation** for `write` and `edit`
   - persisted after validation and before the mutation boundary;
   - records the resolved target identity;
   - records whether the target was absent or its strong pre-state digest;
   - records the intended strong post-state digest; and
   - for `write`, records any missing parent path whose creation is part of the
     operation.

3. **Finished**
   - persisted as part of making the tool result recoverable, before the
     harness considers the call complete;
   - records bounded completion evidence sufficient to reconstruct or
     synthesize a truthful function-call output; and
   - does not need to expose lifecycle bookkeeping to the model on ordinary
     successful turns.

Use a collision-resistant internal digest over the complete target bytes. Do
not reuse short display hashes or make the model manage the recovery identity.

### Resume reconciliation

Cold resume should combine the saved model call, lifecycle records, and current
filesystem state to derive one of a small number of outcomes:

| Durable/current evidence | Derived recovery state |
| --- | --- |
| No `Started` record | `not_started` |
| Read started, no result | `interrupted_without_workspace_mutation` |
| Durable finished result | `completed` |
| Prepared file target exactly matches post-state digest | `intended_state_present_without_recorded_result` |
| Prepared `edit` target exactly matches pre-state digest | `intended_state_absent` |
| Prepared `write` target matches pre-state or remains absent | Intended file state is absent; report any possible parent-directory residue |
| Prepared target matches neither pre-state nor post-state | `outcome_unknown` because another actor or later mutation changed it |
| Bash started without a durable finish | `outcome_unknown` |

Without a durable `Finished` record, a matching post-state proves that the
intended bytes are present now; it does not prove which actor wrote them. A
matching pre-state proves the intended bytes are absent now; it does not prove
the tool never committed and was later reverted. If a `write` call's pre-state
and post-state are identical, recovery can say only that the desired state is
already present. Recovery should state current reconciled facts and must not
invent historical attribution.

The normalized function-call output should then state only the derived fact and
the smallest safe next action. A generic turn-level warning may still be useful
when any opaque outcome remains, but exact file-call evidence should not be
discarded behind one blanket `aborted` label.

Any production wording would be model-facing context and requires explicit user
approval under `AGENTS.md`. This case study authorizes no such change.

### Do not automatically roll back workspace work

The paper's reversible-effect vocabulary must not be applied mechanically to
agent edits.

A `write` or `edit` result is usually the requested work product, not a temporary
resource that should disappear when a process crashes. Automatically restoring
the pre-image could destroy valid completed work and conflict with user or
concurrent changes.

The proposed ledger therefore supports **reconciliation**, not automatic undo:

- clean up only harness-owned acquisitions such as live process groups and
  temporary files;
- preserve current workspace state, including completed requested mutations;
- classify exact file state when possible;
- report opaque Bash effects as unknown; and
- require explicit compensation for genuinely irreversible external actions.

This is a deliberate adaptation of the paper's system-boundary lesson, not a
weaker implementation of universal rollback.

### Why this is a net harness improvement

The proposal closes a concrete correctness gap without expanding the model's
ordinary capability surface:

- no fifth tool;
- no plugin system;
- no dynamic schema;
- no extra successful-turn prompt item;
- no model-managed transaction token; and
- no claim that arbitrary commands are reversible.

It also reduces the chance that the model repeats a destructive command merely
because the prior output is missing.

The tradeoff is additional session-journal writes and recovery code. If process
interruptions are rare, the aggregate compute and storage efficiency may be
slightly worse even though recovery quality is better. The feature should be
adopted for correctness first and credited with efficiency only if measured
interrupted sessions require fewer inspections, retries, tokens, or tool calls.

## Context, tool, and token efficiency

| Dimension | Expected effect |
| --- | --- |
| Successful-turn model context | Neutral if lifecycle records remain internal |
| Successful-turn input/output tokens | Neutral |
| Successful-turn tool calls | Neutral |
| Local runtime work | Slightly worse because of bounded journal records and file digests |
| Cold-resume context | Slightly more precise, not necessarily smaller |
| Cold-resume tool calls | Better when exact status avoids inspection or duplicate execution |
| Cold-resume tokens | Better only to the extent that recovery becomes shorter and requires fewer tool loops |
| Reliability | Materially better if status classification is exact |

The paper therefore does not justify describing the primary proposal as a
steady-state token optimization. Its efficiency benefit is conditional on
interruption frequency and avoided recovery work.

## Secondary candidate: content-addressed skill activation

The paper's spatial model separates a provider's identity from a dependent's
current activation. That suggests a possible context optimization for
bettercodex skills:

1. inject a selected `SKILL.md` definition once per content hash and history
   lineage;
2. on later turns, activate the existing definition with a small turn-scoped
   reference;
3. inject a new full definition when the content hash changes; and
4. after compaction, restore full definitions only for skills whose activation
   must survive into the compacted continuation.

The mechanism could avoid retransmitting an unchanged skill body every time a
frequently used skill is selected. It would preserve progressive disclosure:
only selected skills receive full definitions.

This is not yet a recommendation because several countervailing effects may
erase or reverse the gain:

- Responses prompt caching may already amortize stable prior content;
- a short reference may weaken instruction salience or model adherence;
- an old definition remains in history across intervening turns and could be
  confused with current activation;
- normal-turn history must remain incremental under `docs/inference.md`;
- compaction must restore exactly the active definition/activation relation;
- saved sessions and cold resume need a stable content-identity contract; and
- the activation marker and any recovery error are model-facing behavior.

Evaluate this separately with repeated skill use, cached versus uncached input
tokens, task adherence, compaction, and cold resume. Do not couple it to the
tool-recovery experiment, because that would confound two unrelated mechanisms.

## Rejected ports

### Do not port Cordis as bettercodex's runtime

Rust ownership and the fixed architecture already provide cheaper lifecycle
guarantees for the resources bettercodex owns. A dynamic context/effect graph
would add implementation and reasoning cost without a demonstrated product
need.

### Do not add a plugin, MCP, or configuration framework

DeepSeek Harness's extensibility serves a different product. bettercodex is a
focused Codex port with four ordinary tools and hosted web search. This paper
does not supply a concrete bettercodex use that overrides
`docs/product-direction.md`.

### Do not make the tool catalogue dynamic or self-modifying

The paper motivates self-evolving harnesses as an application, but it does not
show that model-directed tool installation improves quality, safety, or
resource use. Dynamic discovery would also spend context and introduce new
trust boundaries.

### Do not treat dependency declaration as permission enforcement

Coeffects describe activation conditions. They do not sandbox commands or
remove ambient process authority. bettercodex must continue to state its actual
security boundary rather than relabeling dependencies as capabilities.

### Do not promise rollback for Bash

A command can edit many files, start services, alter repositories, install
packages, send network requests, or mutate remote systems. Process termination
is cleanup of a live acquisition, not reversal of emitted effects.

### Do not rewrite ordinary history to deduplicate context

Desired-state reconciliation is appropriate for generated world-state items and
transactional compaction. Rewriting earlier normal history to remove repeated
skill bodies would violate the stable-history and prompt-caching discipline in
`docs/inference.md`. Any skill optimization must work through incremental
activation semantics or compaction, not opportunistic history surgery.

## Required evaluation for phase-aware recovery

A future implementation session should treat this document as an assignment to
build a task-owned prototype, not permission to ship.

### Governing sources

Read before designing the prototype:

- [`AGENTS.md`](../AGENTS.md)
- [`docs/product-direction.md`](../docs/product-direction.md)
- [`docs/development.md`](../docs/development.md)
- [`docs/inference.md`](../docs/inference.md)
- [`docs/model-facing-context.md`](../docs/model-facing-context.md)
- this case study and its local PDF

Temporarily clone current upstream Codex, inspect its then-current tool
execution, rollout, normalization, cancellation, and resume behavior, and remove
the clone afterward. If upstream has adopted equivalent lifecycle records, port
that implementation instead of inventing a divergent one.

### Variants

1. **A — current recovery**
   - turn-level started/finished records;
   - tool calls and completed outputs in history;
   - synthetic `aborted` for missing outputs;
   - generic interruption guidance.

2. **B — internal effect-aware recovery**
   - the same model prompt and tool catalogue;
   - fixed internal effect classes;
   - durable started/prepared/finished evidence;
   - deterministic resume reconciliation; and
   - bounded tool-specific recovery outputs only for unresolved calls.

Build both variants from the same source revision, profile, and toolchain.

### Fault-injection boundaries

Exercise at least:

1. after a model tool call is persisted but before execution starts;
2. after `Started` but before file preparation;
3. after mutation preparation but before atomic replacement;
4. after atomic replacement but before durable completion;
5. after durable completion but before ordinary history projection;
6. after `write` creates missing parents but before target replacement;
7. after Bash starts but before it exits;
8. after Bash exits but before its result becomes recoverable;
9. while several read/Bash calls are executing in parallel;
10. after an external actor changes a prepared target; and
11. through cold resume, a subsequent turn, compaction, save, and another
    resume.

Use process termination rather than only returned errors. Returned errors cannot
prove crash-window behavior.

### Deterministic checks

For every injected boundary, assert:

- the JSONL tail remains repairable;
- history contains a valid output for every retained call before the next model
  request;
- the recovery status matches durable evidence and current target bytes;
- a target matching the intended post-state is never told to retry blindly;
- a target matching the exact pre-state is never reported as durably completed;
- an ambiguous or externally changed target is never guessed;
- current file state is never misrepresented as proof of which actor wrote it;
- Bash without durable completion remains unknown;
- no recovery path automatically restores workspace bytes;
- temporary files and live process groups receive the strongest cleanup the
  process-lifetime boundary permits; and
- successful, uninterrupted turns produce the same model-visible history as the
  baseline.

### Model evaluation

After deterministic fault checks pass, run matched GPT-5.6 Sol recovery tasks
where the next turn must decide whether to inspect, retry, or continue.

Score correctness before efficiency:

- duplicate destructive actions;
- skipped required actions;
- unintended rollback;
- final workspace correctness;
- recovery tool calls;
- inspection reads;
- Bash retries;
- input, cached-input, output, reasoning-output, and total tokens; and
- elapsed time from resume to a correct quiescent state.

A model evaluation is necessary because a mechanically precise recovery record
can still be phrased in a way that causes poor model behavior. Any proposed
model-visible wording must be approved before production editing.

### Runtime metrics

Measure the candidate's normal-path cost separately:

- journal records and bytes per tool class;
- digest time by file size;
- added latency before mutation;
- added latency before the next model request;
- peak memory;
- session-load time; and
- behavior when the final journal record is itself interrupted.

Predeclare an acceptable overhead threshold before running. Do not justify an
arbitrary amount of normal-path I/O with a rare recovery win discovered after
the fact.

### Adoption gates

Do not ship unless all of these hold:

1. **Truthful recovery**
   - every deterministic file case is classified correctly;
   - every opaque case remains explicitly unknown.

2. **No successful-turn context regression**
   - the ordinary tool catalogue and model-visible history remain unchanged;
   - internal lifecycle records never leak into prompts.

3. **No destructive compensation**
   - resume preserves current requested work that reached its intended state;
   - automatic cleanup is limited to harness-owned acquisitions.

4. **Bounded cost**
   - record size and digest work are bounded;
   - representative successful turns show no material latency regression.

5. **Recovery benefit**
   - matched interrupted tasks avoid duplicate actions or materially reduce
     recovery calls/tokens without lowering final correctness.

If deterministic correctness improves but the model evaluation shows no token
or call reduction, report it as a reliability improvement rather than an
efficiency improvement.

## Durable engineering principles to carry forward

Even if the proposed ledger is rejected, the following review rules are worth
retaining:

1. **Name the lifecycle states.** Missing output is not a state description.
2. **Acquire with an owner.** Every runtime-owned resource needs a local cleanup
   path that covers partial failure.
3. **Separate withdrawal from destruction.** Stop new use, drain dependents,
   then tear down the provider.
4. **Reconcile rather than replay.** On resume, derive current state from
   durable evidence and the live world instead of blindly repeating actions.
5. **Respect the system boundary.** Internal cleanup, workspace mutation, and
   external emission require different semantics.
6. **Keep declarations honest.** Dependency metadata is not permission
   enforcement.
7. **Measure the whole harness.** A locally elegant abstraction is not a token,
   latency, or task-quality win until evaluated end to end.

## Suggested assignment for a future agent

> Read
> `case-studies/deepseek-spatiotemporal-composability-harness-learnings.md`
> and its adjacent PDF. Evaluate a minimal internal lifecycle journal for
> bettercodex's existing four tools. Keep successful-turn model context
> unchanged. Use deterministic process-crash fault injection first, then a
> matched GPT-5.6 Sol recovery evaluation. Treat `read` as read-only,
> `write`/`edit` as atomic target mutations with strong pre/post evidence, and
> `bash` as an opaque emission. Never automatically roll back workspace or
> external effects. Compare current upstream Codex before implementing, remove
> temporary artifacts afterward, and return evidence before proposing a
> production change.

## Conclusion

DeepSeek's paper does not give bettercodex a reason to become an extensible
Cordis application. It gives bettercodex a sharper rule for one existing weak
point:

> Durable recovery should record and reconcile lifecycle facts, not collapse
> every missing tool output into `aborted`.

That is the smallest plausible net harness improvement in the paper. It fits
bettercodex's fixed architecture, preserves model-context economy on successful
turns, and targets a real ambiguity at the exact boundary where tools can change
the world before the conversation records what happened.
