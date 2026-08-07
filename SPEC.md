# Proactive Engineering Quality

## Status and agreement rule

This specification records the product contract produced through an extensive,
rigorous back-and-forth discussion. It contains only decisions we agreed on or
details completed under the operator's instruction to finish the faithful
`autoresearch` port. An idea does not enter this file merely because one of us
proposed it.

Apply [`docs/writing-instructions.md`](docs/writing-instructions.md) throughout
this process. Preserve the original bluntness, examples, emphasis, and causal
reasoning instead of professionalizing them into vague product language.

## Why this exists

We are approaching bettercodex wrong if we expect an `# Engineering
ownership` block in [`prompts/system.md`](prompts/system.md) to magically make a
general coding agent proactive.

The agent is not deterministic. It drifts, and while doing a task it is focused
on getting that task done. A broad prompt telling the same agent to notice every
quality problem, widen its ownership, clean everything up, validate everything,
and only then stop may help a little, but it does not make that behavior
reliable. Repeating and enlarging the instruction also spends permanent context
on a behavior the harness still cannot guarantee.

This does not mean prompts are useless. The main agent still needs a compact
contract for autonomy, scope, authority, and user control. It means that
repeatable engineering quality cannot depend on the main agent remembering a
large motivational paragraph at the right moment.

Engineering ownership must be a workflow, not a personality trait.

## Goal

Stop thousands of ordinary bettercodex sessions from quietly compounding
technical debt, slop, poor implementation, and scope creep.

Make exceptional engineering more repeatable with an opt-in quality loop. Build
one evaluator, run one fresh working session, let it finish, then start the next
fresh working session over the same task and working tree. Keep improvements,
discard regressions, and repeat until the requested count is exhausted.

The harness owns whether those steps run. The model still owns the judgment
inside each step. "Deterministic" therefore means deterministic orchestration,
not a claim that an audit model itself becomes deterministic.

## Keep the main agent lean

The removed `# Engineering ownership` block will not be restored as the primary
proactivity mechanism.

The main prompt should state each durable harness rule once and stay focused on
the general coding loop. Bespoke quality work belongs in a bespoke stage with
clear evidence, scope, and success criteria.

This direction follows [`docs/5.6-prompting.md`](docs/5.6-prompting.md) and the
live [GPT-5.6 Sol prompting
guidance](https://developers.openai.com/api/docs/guides/prompt-guidance-gpt-5p6).
That guidance reports better coding-agent results from leaner prompts: state the
outcome, important constraints, evidence, and completion bar once, then leave
room for the model to choose an efficient path. The two phase prompts below use
that shape.

## The quality loop

The quality loop is optional, not the default behavior for every task. The
operator can invoke the same loop in either of two ways:

- mention `$loop` anywhere in the task prompt; or
- use the `/loop` slash command.

This is one feature with two entry points, not two implementations. An inline
skill mention matters because the operator may realize midway through writing a
prompt that this particular task deserves the loop and should not have to
rewrite the prompt around a slash command.

`/loop` is a prompt prefix: `/loop <task>` uses the default count and
`/loop 8x <task>` supplies a count. `$loop` can appear anywhere in an otherwise
ordinary task prompt. A trigger applies only to that submission; it is not a
mode or a toggle for later prompts. A trigger without task text or an attachment
is rejected. Both names use exact token boundaries: `$looper` and `/looping` are
ordinary task text, not near-match triggers.

`/loop` is recognized only as the first non-whitespace command token. `$loop`
uses the same bounded ASCII skill-name tokenization as other explicit skill
mentions, but quoted prose and fenced code are not exempt from trigger parsing.
Triggers and counts are parsed only from submitted text; attachments participate
only in the non-empty-task check. The grammar does not depend on how the TUI
happened to color or complete a token.

Both entry points are resolved by the harness before a normal agent turn is
submitted and produce the same loop request. The trigger is consumed exactly
once. The original operator message remains verbatim in the task record, but
replaying that evidence inside a fresh session must not recursively start
another loop.

The frozen task record is the ordered sequence of original operator-authored
user inputs in the active parent session through the invoking submission. It
does not include synthetic repository context, assistant messages, tool output,
or a model-selected summary. The harness does not guess which earlier operator
sentence still matters; later user inputs retain their normal ability to refine
or supersede earlier ones at the same authority. If the verbatim record cannot
fit a fresh session, invocation fails instead of compacting it.

Attachment payloads, detail settings, and positions are captured with that
record at invocation. A later fresh session never rereads an attachment from a
mutable source path. Existing per-item, attachment-size, and context limits
still apply; the loop rejects input it cannot persist and replay exactly rather
than dropping, recompressing, or summarizing it.

Submitting a valid loop request is confirmation to build the evaluator and run
the requested working-session count. Do not add another setup confirmation
pause. Existing approval boundaries still apply to actions that would require
approval outside the loop.

The default loop count is `3`. The operator can override it with trigger-local
forms such as `$loop 5 times`, `$loop 5 iterations`, `$loop 5x`, `5x $loop`, or
`/loop 8x`. Whitespace and ordinary punctuation may separate the trigger and
count, but an unrelated number elsewhere in the task is task content, not loop
control. Count extraction is a bounded harness parser, not a model inference.
It applies only to working sessions; the evaluator session is setup and does
not consume an iteration.

The count grammar is closed: a count phrase is a base-10 integer followed by
`times`, `iterations`, or `x`, with no intervening task word. It may immediately
precede or follow inline `$loop`; a `/loop` count appears immediately after the
command and before the task. A bare integer is never loop control. Recognized
trigger and count spans do not themselves satisfy the non-empty-task rule.

An explicit count must be a positive base-10 integer. Fractions, signs,
exponents, overflow, zero, malformed count-like text in the trigger-local slot,
or conflicting trigger-local counts are rejected before a run directory is
created or a model session is started. Repeated triggers with the same explicit
count still create one run. Do not guess, silently clamp, or choose between
conflicting counts.

If repeated triggers contain one unique explicit count, triggers without a
count do not conflict with it. Two different explicit values are conflicting;
repeating one value does not multiply the count.

The default `$loop` therefore runs one evaluator session followed by three
working sessions. `$loop 5 times` runs one evaluator session followed by five
working sessions. The first working session implements the task against the
evaluation package. Every later working session inspects, edits, and reevaluates
the result produced so far.

The loop begins with a fresh session whose entire context is dedicated to
constructing the evaluator before any loop working session changes the
implementation. That session does not implement the operator's task. It must
start from
[`docs/evals/MANIFEST.md`](docs/evals/MANIFEST.md) and follow only the live
routes needed for the task. Like every working iteration, it uses
`gpt-5.6-sol` at `max` reasoning effort.

The evaluator framework must be agnostic; the evaluator it produces must be
specific to the task. It must infer from the operator's intent and repository
evidence what genuine improvement means here. Depending on the task, that could
turn on performance, efficiency, footprint, simplicity, behavior, usability,
maintainability, or something else entirely. These are examples, not a fixed
checklist. Do not force every task through every dimension or assume in advance
which one matters.

The evaluator session must produce a small, runnable evaluation package, not a
universal quality score and not merely a prose rubric. It must:

1. reduce the operator's intent to the smallest set of concrete promises needed
   to represent the task;
2. distinguish hard acceptance requirements, the property being improved, and
   affected behavior or qualities that must not regress, then identify the
   specific failure each promise must catch;
3. define the candidate's editable boundary and the fixed evaluator, oracle,
   budget, permission, and repository constraints that no candidate may change;
4. map every promise to focused evidence, allowing one check to cover several
   inseparable promises instead of duplicating work;
5. choose the cheapest trustworthy evidence for each promise, preferring
   existing tests, builds, benchmarks, linters, static checks, schemas, and
   direct artifact inspection before adding model judgment;
6. add only the focused normal, edge, or adversarial cases that existing
   evidence cannot supply, including a shortcut or grader-hacking case when the
   identified failure mode makes one relevant;
7. record commands, inputs, environment assumptions, writable scratch paths,
   timeouts, resource budgets, result extraction, and how ties, measurement
   noise, and inconclusive results are handled;
8. run the evaluator against the exact starting state before production
   behavior changes and verify that its decisive cases can discriminate the
   failure they claim to catch;
9. define both when a candidate is acceptable and when it is genuinely better
   than the preceding result without regressing the hard promises; and
10. state what remains uncovered, uncertain, or impossible to evaluate
   reliably.

When a criterion genuinely requires model judgment, prefer pass/fail or
pairwise comparison over an invented numerical score. Give the rubric concrete
evidence requirements, control for position and verbosity bias when relevant,
and calibrate it with trusted contrasting examples when possible. An
uncalibrated model judgment cannot become a hard gate merely because no direct
check was convenient. If no trustworthy evaluator can be built for a decisive
property, say so instead of manufacturing a metric.

When trusted labeled cases are numerous enough to calibrate a model judge, keep
prompt examples, tuning cases, and a held-out check distinct. Report how well it
catches known failures and preserves known passes separately; aggregate accuracy
can hide a judge that always chooses the majority label. Examples invented and
labeled only by the same judge are not human or expert calibration.

Passing the evaluation package proves only the promises it covers. It is
evidence, not proof that the whole implementation is good, and a change does not
pass merely because its author explains why it should.

Evaluator setup succeeds only after the runnable package has been written,
validated from outside the evaluator session, and its machine-checkable portion
has run against the restored starting state. A prose-only response, a claimed
but unreproduced machine baseline, a package whose decisive case does not detect
its stated failure, or a package with no usable acceptance and comparison
procedure is a setup failure. The harness restores every evaluator-session
change outside its provided evaluator workspace; a package that depends on one
of those changes is invalid.

### Evaluator-creation prompt

After loading the task, repository context, and the evaluator documentation
routes above, the dedicated evaluator session receives this prompt:

```markdown
# Build the task evaluator

Your only job is to build and baseline the local evaluator that later loop
sessions will use. Do not implement the operator's requested change. Persist
files only inside the provided evaluator workspace in the run directory.

Start with the provided eval documentation manifest. Read Evaluation Best
Practices first, then follow only the live routes needed for this task.

Infer the intended outcome from the operator's messages, applicable repository
instructions, current behavior, and authoritative sources when needed. Choose a
concrete one- or two-word loop name from the task's subject and target.

Success means the frozen package can answer two questions with evidence:

1. Is this candidate acceptable?
2. Is it genuinely better than the preceding incumbent?

Reduce the outcome to the smallest set of concrete promises that can answer
those questions. For every promise, state the failure it must catch and classify
it as an acceptance requirement, the property being improved, or a regression
boundary. Define what candidates may change and what evaluator, oracle, budget,
permission, and repository constraints remain fixed.

Choose the cheapest trustworthy evidence. Prefer existing executable checks,
direct artifact inspection, tests, measurements, invariants, and schemas. Add
focused cases only where existing evidence is insufficient. Exercise the path
that matters to the operator; do not substitute a convenient proxy. Add normal,
edge, adversarial, or shortcut cases only when the identified failures justify
them.

Use model judgment only when direct evidence cannot settle a promise. Prefer
pass/fail or pairwise comparison, require concrete artifact evidence, and use
trusted contrasting examples for calibration when available. Control position
and verbosity bias when they can affect the decision. Do not turn an
uncalibrated opinion into a hard gate or combine unrelated dimensions into an
invented score.

If enough trusted labels exist, separate prompt examples, tuning cases, and a
held-out check. Measure known failures caught and known passes preserved instead
of relying on aggregate accuracy.

Record every command, input, expected result, result extractor, writable scratch
path, timeout, resource budget, and environment assumption. For noisy
measurements, establish enough baseline evidence to define a frozen tolerance
or inconclusive outcome. Define how ties are resolved; simplicity is a
tie-breaker only when the evidence and comparison rule make it one.

Build the complete package, then evaluate the exact starting state before any
implementation work. Confirm that decisive cases detect the failures they claim
to catch and inspect the package for obvious ways a candidate could game the
ruler.

Write the required machine-readable contract and human-readable rationale,
including the loop name, promise map, candidate boundary, protected evaluator
assets, checks, cases, baseline, acceptance rule, comparison rule, required
evidence, and known blind spots. Do not prescribe one implementation when
several could satisfy the same promises.

If the real outcome cannot be evaluated reliably, stop and report the exact gap
instead of manufacturing a proxy. End the final response with exactly this
four-line envelope, using `none` where a field does not apply:

SETUP: <READY|BLOCKED>
CONTRACT: <run-relative contract path>
BASELINE: <run-relative baseline evidence path>
BLOCKER: <none or concise evidence-backed blocker>
```

The harness reads only that terminal envelope as phase control. `READY` is
valid only when the contract and baseline paths resolve inside the current run,
match the captured starting state, and `BLOCKER` is `none`. `BLOCKED` requires a
non-empty evidence-backed blocker; any partial package remains diagnostic only
and is never frozen.

### Evaluation package contract

The package has one small machine-readable contract plus any focused scripts,
fixtures, rubrics, references, and baseline output it names. The harness owns
the contract schema and validates it before accepting setup; this is internal
run state, not a new bettercodex configuration framework.

The contract records, at minimum:

- the loop name and stable identifier for every promise;
- each promise's class, statement, concrete failure mode, evaluator method, and
  required evidence;
- the candidate's allowed mutation boundary and every inherited constraint that
  remains fixed;
- every machine check's argument vector, working directory, controlled
  environment additions, input and fixture paths, timeout, resource budget,
  side-effect and approval classification, expected termination, and result
  extraction rule;
- every model-judged check's frozen pass/fail or pairwise rubric, required
  artifact evidence, calibration examples when available, and output shape;
- the protected evaluator integrity set, declared evaluator scratch paths, and
  environment identities needed to tell a comparable run from drift;
- the baseline result for every check, including repeats or observed variance
  when the metric is noisy;
- an explicit acceptance rule and incumbent-comparison rule, including tie,
  noise, and inconclusive behavior; and
- uncovered properties, assumptions, and known grader loopholes.

The candidate boundary is a closed mutation set, not a suggestion. It cannot
overlap run control state or any evaluator or repository-owned path in the
integrity set. If a task must edit a repository file that currently supplies an
oracle, that oracle cannot also remain a frozen gate; the evaluator must move
the independent oracle into its package, use other evidence, or classify the
check as supplemental.

Contract-owned files are relative to the run root. References to the candidate
use an explicit worktree root plus a normalized relative path. Neither kind can
escape its root through `..`, an absolute path, or a symlink. Commands are
represented as argument vectors rather than shell text unless shell semantics
are themselves required and declared. Every output is bounded; full logs go to
the baseline or iteration evidence directory and the contract's result
extractor returns only the decisive summary.

The harness schema also bounds package bytes, file and check counts, argument
and environment sizes, path lengths, fixtures, and extracted results. Exceeding
a bound is a visible setup failure, never a reason to truncate an oracle or
silently skip a case.

Automatic machine gates must be repeatable local validation. A check that can
write externally, destroy data, purchase resources, or cross another existing
approval boundary cannot become an automatic gate merely because the evaluator
declares it. It requires the same separate authority as an ordinary session; if
that authority is indispensable and unavailable, evaluator setup reports the
blocker.

The provisional candidate itself is confined to declared, recoverable worktree
and Git surfaces. An external service, account, deployment, purchase, message,
or data mutation cannot be candidate state because `DISCARD` cannot restore it.
Read-only external evidence still follows normal authority and permission
rules. A task whose outcome intrinsically requires an irreversible or repeated
external action is blocked for a separate operator-controlled workflow instead
of pretending the quality loop can roll it back.

Declared evaluator scratch paths cannot overlap the candidate boundary or the
integrity set. Their pre-run state is restored after every evaluation and before
a candidate identity is captured, including evaluator runs initiated by the
worker. A cache, generated artifact, or log therefore cannot become candidate
work by accident. If a check needs persistent state between runs, that state is
an explicit frozen input with a reset procedure, not scratch.

An existing repository test, benchmark, schema, or script can be a frozen gate
only when the oracle-bearing parts that give it meaning are in the integrity
set. Mutable production code under test is not protected. If the package cannot
separate a repository-owned oracle from code the task must be allowed to edit,
that check is supplemental evidence rather than the sole acceptance gate. This
is the generic equivalent of `autoresearch` keeping `prepare.py` fixed while
allowing `train.py` to change.

Before the evaluator session starts, the harness snapshots the starting state.
After the session finishes, it preserves the evaluator-workspace package, restores
every change outside that directory, validates and freezes the package, and
runs all declared machine checks itself against that restored state. The
evaluator session's prose and self-reported command results are not the machine
baseline. A setup package that cannot reproduce that machine baseline in this
clean pass is rejected.

For a genuine model-judged check, the evaluator session's structured rubric
artifact is the baseline judgment. It is bound to the starting-state identity
and kept distinct from machine results; the harness can validate its schema,
rubric, cited artifacts, and calibration evidence but must not claim to have
reproduced the semantic judgment. That nondeterminism is a declared limitation,
not a reason to relabel opinion as a machine gate.

### Frozen evaluator

Once the evaluator has been built and baselined, it is frozen for the duration
of that loop. Every loop session may read and run it, but no loop session may
modify or weaken it.

The same promises, fixtures, commands, inputs, budgets, environment assumptions,
acceptance rule, and comparison rule apply to every candidate. A candidate may
change production code and dependencies when the task permits; it may not change
the ruler used to judge those changes.

This boundary is enforced by the harness, not entrusted to another prompt. The
harness records the complete protected path set, file types, modes, bytes, and
contract, then verifies their integrity throughout the loop. Adding, deleting,
replacing, or redirecting a protected path is tampering even when a content hash
happens to be unchanged elsewhere.

The package distinguishes checks the harness can run and verify from judgments
the working session must make under a frozen rubric. The harness reruns every
declared machine-checkable gate before accepting `KEEP`. A criterion that truly
requires model judgment remains the working session's judgment; the harness
requires its structured evidence artifact and verifies that the frozen rule was
used, but does not silently create another judge session. An explanation from
the candidate's author is not a substitute for that artifact.

If the evaluator materially contradicts the operator's request or cannot judge
the task reliably, the loop stops and reports the problem. A fresh evaluator
session must rebuild and baseline it before implementation or improvement can
continue. That rebuild begins a new run; the current evaluator is never unfrozen
in place. Silently moving the goalposts is forbidden.

### Repository-local run state

The loop engine belongs in bettercodex, but loop runs do not belong in the
global bettercodex installation or global saved-session storage.

The loop requires an active Git worktree and uses its nearest worktree root. If
bettercodex cannot find one, invocation fails before creating state or starting
a session. This preserves the same repository-and-Git boundary that
`autoresearch` relies on.

Preflight also rejects a tracked `.bcodex/loops` path, a non-directory or
symlink at any loop-state path component, an unreadable worktree, or recovery
state that cannot be reconciled safely. Run directories and files are private
to the invoking user on the supported Unix platforms. Loop evidence can contain
task text, attachments, diffs, command output, and secrets already exposed to
the session; ignoring it in Git does not make it public-safe.

Invoking the loop creates `.bcodex/loops/` at the repository root. Every
invocation creates its own `.bcodex/loops/<run-id>/` subdirectory. The evaluator
package, baseline, protected evaluator assets, and later iteration records for
that run live together in that run subdirectory. Consecutive sessions use the
same repository-local run state and working tree.

Each run directory contains, at minimum:

- the operator's task messages in their original order and content, including
  attachments and structured non-loop skill selections, plus separate
  consumed-trigger metadata;
- the invocation-time applicable repository instruction items and explicitly
  selected skill contexts with their original provenance and roles;
- the loop protocol and evaluator-contract versions, exact phase-prompt
  identities, bettercodex build identity, model, and reasoning setting;
- the frozen one- or two-word display name chosen during evaluator setup;
- the starting-state identity and recovery data;
- the validated evaluator contract, frozen package, integrity record, and
  baseline evidence;
- `results.tsv`;
- one evidence directory for every attempted working iteration, including the
  candidate's state identity, created, modified, and deleted files, concise
  hypothesis, evaluator output, final verdict, usage, and timing; and
- the small amount of durable state needed to distinguish setup, an active
  iteration, restoration, and completion after an interruption.

The run directory is loop control state, not part of a candidate. It must be
excluded from candidate diffs, comparisons, and restoration.

The harness is the only authorized writer of control state, the task record,
the frozen contract and baseline, `results.tsv`, and completed iteration
evidence. The evaluator session is authorized one writable setup workspace. An
active worker is authorized one writable evidence directory for its own
structured result and logs. All other run-state paths are logically immutable.
bettercodex remains unsandboxed and phase commands still run with the invoking
user's permissions; these mutation boundaries are enforced by snapshots,
validation, restoration, and tamper detection, not described as an OS security
boundary. Changing an earlier row or evidence artifact is state tampering, not
a candidate edit.

Cold resume validates those protocol, schema, prompt, runtime, model, and
reasoning identities before continuing. If the current binary cannot interpret
the frozen run without changing its contract, recovery blocks and preserves the
run for a new invocation; it never silently migrates the ruler or phase prompt.

Every recorded state identity is a canonical digest of the protected state, not
just `HEAD`. It covers the current branch or detached-HEAD identity, `HEAD`, the
index, and candidate path types, permission modes, symlink targets, and bytes,
including tracked, untracked, and relevant ignored paths. It excludes the run
directory and metadata such as mtimes that do not affect the candidate. The same
definition is used for baseline, incumbent, candidate, restoration, and
recovery checks.

An ignored path becomes candidate state only when the frozen candidate boundary
explicitly includes it. Every other ignored path touched by an evaluator or
worker is side-effect and recovery state: restore its pre-image, or remove it if
the phase created it, before recording a candidate identity or accepting
`KEEP`. The harness retains enough starting inventory or mutation evidence to
detect deletion of a pre-existing ignored path; "ignored" never means
disposable operator data.

Exact recovery does not justify copying the whole repository once per
iteration. Recovery may use content-addressed pre-images, copy-on-write state,
or another representation, but it must capture a restorable pre-image before a
worker can mutate it. Unchanged data may be deduplicated across the run and must
not be copied into every iteration directory. Storage economy cannot weaken the
dirty-worktree guarantee.

`.bcodex/loops/` is ignored by Git by default. Loop state remains visible and
inspectable on disk without appearing as repository work that should be
committed. Install that rule through repository-local Git exclusion; do not edit
the project's tracked `.gitignore` merely to run a loop. Preserve pre-existing
exclude contents and make installation idempotent.

The exact harness-owned exclude entry and worktree lock metadata are control
state, not candidate Git mutations. Normalize them out of state identities and
restoration while preserving every pre-existing exclude entry byte-for-byte.

Global session state may record the repository path and run identifier needed
to resume the session, but it must not duplicate the evaluation package or
accumulate the run's artifacts globally. Evaluator and working sessions must not
appear as independent global saved sessions or leave resumable conversation
transcripts for later workers. Only the task, repository changes, evaluator
output, verdict, usage, timing, and failure evidence needed to audit the run are
retained repository-locally.

Completed runs remain in their repository-local directories until the operator
removes them. bettercodex does not silently prune experiment evidence.

### Experiment ledger

Every run has one compact, append-only `results.tsv`, directly following
`autoresearch`'s ledger pattern. Its columns are:

```text
iteration	state	result	status	description	evidence
```

- `iteration` is `0` for the starting baseline and `1..N` for working sessions.
- `state` is the stable identifier of the evaluated repository state.
- `result` is the shortest useful rendering of the evaluator's decisive result.
- `status` is `baseline`, `keep`, `discard`, `crash`, `blocked`, or
  `interrupted`.
- `description` is one factual line naming the coherent hypothesis and what the
  attempt changed, not a narrative handoff from the previous agent.
- `evidence` is a run-relative path to the complete evaluator output and change
  record for that row.

The baseline row is written before the first working session. Each later row is
written only after its evidence and keep-or-restore result are durable. Existing
rows are never rewritten. Tabs and newlines in field values must be escaped so
one attempt always occupies one row. Use `\\`, `\t`, and `\n` for a literal
backslash, tab, and newline respectively.

Publishing a row is an idempotent transaction keyed by iteration. Recovery may
remove a torn trailing fragment, which was never a complete row, but it never
edits or duplicates a valid published row. The durable phase record decides
whether recovery must finish restoration, publish the prepared row, or advance
to the next iteration.

The ledger carries just enough information for a fresh session to see what won,
what failed, and where the proof lives. Full diffs, evaluator logs, decisive
command output, and other clean evidence stay in the referenced evidence
directory instead of being poured into the next context. When output is large,
write it there and read only the decisive result or relevant failure tail,
mirroring `autoresearch`'s `run.log`. Earlier model responses and reasoning are
not handoff evidence.

### Keep or discard each iteration

Every working session begins from the current incumbent state. Before the
session can edit, the harness snapshots that exact state, including changes that
were already present in a dirty working tree. The session's own changes remain
provisional until it finishes evaluating them.

The protected incumbent includes the current branch and `HEAD`, index, tracked
files, untracked files, and every ignored path the session changes. A discard
must restore every protected path and protected Git-state value changed during
the iteration while leaving pre-existing work byte-for-byte intact. Unless the
contract explicitly includes an ignored path in the candidate boundary, its
restoration is a side-effect cleanup rather than a candidate change. The
operator is never required to commit, stash, or clean the worktree before
invoking the loop.

Worktree-local `HEAD`, the current branch, and the index are the ordinary Git
mutation boundary. Changes to other refs, stashes, remotes, repository config,
hooks, linked worktrees, or submodule repositories are valid only when the
operator's task explicitly requires them and the evaluator contract can snapshot,
compare, and restore them. Otherwise a worker must not touch them, and an
observed mutation is a blocking state-integrity failure rather than a candidate
improvement.

Git's object database, reflogs, lock files, and other internal implementation
metadata are not candidate state merely because an ordinary commit or reset
touches them. The harness must not delete shared objects or rewrite reflogs to
fake byte-for-byte `.git` restoration. It restores the declared logical Git
state and records any unavoidable internal residue in recovery evidence.

The same working session performs the complete cycle: inspect the incumbent,
edit it, run the frozen evaluator, and compare the candidate with the incumbent.
There is no separate reviewer or validation agent.

Following `autoresearch`, one iteration should test one coherent, high-leverage
hypothesis. It may make the coupled edits and incidental fixes needed to run
that experiment, but it must not bundle unrelated cleanup merely to make the
diff look busy. The compact ledger lets a fresh worker avoid repeating a known
failure and inspect detailed evidence only when it matters.

Keep the candidate only when it satisfies the evaluator's acceptance rule and
is genuinely better under its comparison rule. Otherwise restore the exact
incumbent state. A failed, crashed, or interrupted working session also restores
its provisional changes without disturbing anything that existed when the
iteration began.

After the worker finishes, the harness restores every declared evaluator scratch
path to its incumbent state, then snapshots the final candidate before harness
validation. It verifies evaluator integrity and reruns every machine gate in a
disposable evaluation layer rooted at that candidate. Test caches, generated
files, logs, and other evaluator side effects are captured as evidence and
removed by restoring the candidate snapshot; they never become a reason to keep
or alter the candidate. Only then does the harness keep that exact candidate or
restore the incumbent. A no-change state cannot be `KEEP`.

Record every iteration's resolved status and supporting evidence in the
repository-local run directory.

Loop sessions run consecutively. Each iteration is one normal, fresh
`gpt-5.6-sol` session at `max` reasoning effort that reviews and edits the result
produced so far. When that session finishes, the next one starts if iterations
remain.

There is no multi-agent orchestration, coordinator, role graph, agent-to-agent
conversation, reviewer artifact, or split between auditing and patching. The
working tree is the result passed from one session to the next.

Every evaluator or working phase owns the foreground and background processes it
starts. Before setup can freeze, an iteration can resolve, restoration can run,
or another worker can start, the harness terminates and reaps every surviving
phase-owned process and confirms that no owned command can keep mutating the
worktree. Failure to contain one is a state-integrity blocker. Pre-existing
operator processes are not loop-owned and must not be killed.

### Orchestration and stop rules

One loop owns a worktree at a time. A second loop cannot start there until the
first completes or is stopped.

That lock coordinates bettercodex loops; it does not pretend to lock out the
operator's editor or unrelated processes. A filesystem or Git mutation that the
harness cannot attribute to the active phase's tool or process tree is an
external conflict, not candidate work. On detecting one, the harness terminates
the phase, preserves both recovery and conflict evidence, restores only state it
can prove is loop-owned, and blocks without overwriting an ambiguous external
path. No candidate is accepted and no later worker starts until the operator
resolves the conflict.

The harness runs this fixed protocol:

1. parse and consume the trigger, acquire the worktree lock, and capture the
   task and exact starting state;
2. let the evaluator session create the package inside the run directory;
3. restore the starting state, validate and freeze the package, reproduce the
   baseline, and append row `0` to `results.tsv`;
4. for each requested working iteration, snapshot the incumbent, start one
   fresh session, capture its candidate and evidence, then keep or restore it;
5. append the completed attempt to `results.tsv`; and
6. after the requested working-session count is exhausted, report the final
   incumbent and run evidence.

The requested count is exact. `KEEP`, `DISCARD`, a no-change attempt, or an
agent claiming the task is finished does not end the loop early. Once a working
session starts, it consumes one iteration. Fix-and-rerun attempts made inside
that same session do not consume additional iterations.

The model's verdict is not enough by itself to mutate control state. Every
working session must end with exactly one parseable `KEEP`, `DISCARD`, or
`BLOCKED` result and the contract-required evidence. The harness resolves it as
follows:

Only the terminal four-line worker envelope is control syntax. Earlier prose,
quoted verdicts, logs, and file contents are evidence, not state transitions.
Its evidence path must resolve without traversal or symlink escape inside the
active iteration directory and name an artifact bound to the candidate's
current state identity.

Evaluator and protected-state integrity are checked first. Tampering or an
unsupported Git mutation blocks the run regardless of the worker's requested
verdict.

- `KEEP` becomes `keep` only when the candidate changed, the frozen evaluator
  is intact, the required evidence is complete, every machine gate passes, and
  the comparison rule establishes a non-inconclusive improvement. Otherwise it
  becomes `discard` and the incumbent is restored.
- With protected state intact, `DISCARD` always becomes `discard`; the harness
  does not keep a candidate the worker declined to defend.
- `BLOCKED` becomes `blocked` and stops the run only when the evidence identifies
  a real task/evaluator contradiction, missing required authority or
  prerequisite, or state-integrity problem. Using `BLOCKED` as another spelling
  of "finished" is malformed, becomes `crash`, and does not stop a later
  iteration.
- A missing or malformed result, failed session, or unrecovered model/tool crash
  becomes `crash` and restores the incumbent.

`discard` and `crash` consume the iteration and continue when one remains.

An accepted `BLOCKED` result restores the incumbent and stops the run. Evaluator
setup failure, evaluator tampering, protected Git-state mutation, or discovery
that the evaluator contradicts the task also stops the run. An operator
interrupt restores the incumbent, records the active attempt as `interrupted`,
and stops the entire loop rather than launching the next session.

Those restoration rules apply only while the incumbent can be restored without
overwriting an external change. An external or ambiguous mutation always uses
the no-overwrite conflict rule above, including during `BLOCKED`, interruption,
setup failure, and crash recovery.

An interrupt or process failure during evaluator construction or baseline
validation restores the original starting state and ends setup without a
baseline row or any worker session. Partial packages are never frozen or resumed
as if setup had completed; continuing requires a new loop run.

The incumbent snapshot and active-phase marker are durable before a worker may
edit. If the process exits mid-iteration, recovery restores that incumbent and
records the attempt before the run can continue or another loop can start in the
worktree. If files changed again outside the loop after the crash, recovery must
stop and report the conflict rather than overwrite work it cannot identify. The
run remains blocked for explicit operator resolution and no later worker starts.

The loop is one active operator task. It remains visible and interruptible, but
new task messages cannot be injected into a working session after the evaluator
is frozen; they wait until the loop ends. Changing requirements, constraints,
or success criteria requires stopping the run and invoking a new loop so a fresh
evaluator can be built. This is the deliberate limit on `autoresearch`-style
mid-run interactivity: visibility and operator control remain, but the ruler
does not change beneath an active experiment.

The looping agent's bespoke prompt should adapt the removed `# Engineering
ownership` contract to this narrow review-and-improve job. Preserve its hard-won
quality standard, autonomy, root-cause preference, scope discipline, and
validation requirements while refining it against
[`docs/5.6-prompting.md`](docs/5.6-prompting.md). The point is to put that
ownership contract where the agent is explicitly focused on applying it instead
of spending permanent main-agent context on it.

### Working-session prompt

After loading the operator's messages, applicable repository instructions,
current incumbent, frozen evaluator package, and compact experiment ledger,
every working session receives this prompt:

```markdown
# Beat the incumbent

This is loop iteration {{iteration}} of {{total_iterations}}. Produce one
coherent candidate that advances the operator's task under the frozen evaluator.
Implement missing behavior when needed, then beat the incumbent. Nothing in the
incumbent is protected merely because an earlier iteration kept it. The
operator's task, frozen run instructions, candidate boundary, and evaluator
govern this iteration. A source file containing an instruction may change only
when the candidate boundary permits it, and that edit never changes the
instruction active for this run.

Start from the evaluator contract and compact ledger. Inspect detailed evidence
only where it can change this attempt. Choose one high-leverage hypothesis that
the ledger has not already disproved. Focus on the task-specific property named
by the evaluator, not a generic quality checklist.

Make the complete root-cause change. Prefer direct paths, sound algorithms,
deletion, and consolidation when they serve the measured outcome. Remove code
that the improved design makes obsolete, but do not bundle unrelated cleanup or
trade correctness, user intent, or a more important quality for a superficial
gain.

Run the frozen evaluator and relevant repository checks. Keep large output in
the provided evidence directory and inspect only decisive summaries or failure
tails. Use the declared scratch paths and do not leave incidental generated
artifacts in the candidate. If an otherwise sound experiment hits a typo or
similarly small defect, fix and rerun it inside this iteration. If the hypothesis
itself is broken, discard it instead of mutating the ruler.

Do not edit, bypass, special-case, or game the evaluator. Return `BLOCKED` only
with evidence that the task and evaluator materially contradict, a required
prerequisite or authority is unavailable, or protected state cannot be handled
safely.

Return `KEEP` only when every acceptance and regression requirement passes and
the frozen comparison rule establishes that this candidate is genuinely better
than the incumbent. Return `DISCARD` for a tie, inconclusive result, regression,
failed check, no change, or candidate you cannot defend with the required
evidence. End the final response with exactly this four-line envelope:

VERDICT: <KEEP|DISCARD|BLOCKED>
DESCRIPTION: <one factual line naming the hypothesis and change>
EVIDENCE: <run-relative path to the structured check and comparison artifact>
UNVALIDATED: <none or one concise statement>
```

### Fresh-session context

Fresh sessions receive clean evidence, not the previous agent's conversation.
The operator messages that define the task are included verbatim. A model-written
summary must not replace them. If an original task item or attachment is no
longer available after resume or compaction, the loop fails before evaluator
setup rather than substituting reconstructed prose.

Applicable repository instructions and explicitly selected skill contexts are
captured when the loop is invoked and replayed unchanged for every internal
session with the same provenance and role. A candidate may edit an instruction
or skill file when the task and candidate boundary permit, but that edited text
is an artifact under evaluation, not a way to rewrite the active run's
instructions. A later ordinary turn or new loop performs normal discovery again.

The task must also remain intelligible without treating an earlier assistant
proposal as operator-authored requirements. A submission such as "do that
`$loop`" can rely on operator messages and named on-disk artifacts, but if its
only definition of "that" lives in excluded assistant prose, evaluator setup
reports the missing task contract. The harness never promotes an assistant
handoff or compaction summary to user authority just to make the loop proceed.

The rest of the handoff follows `autoresearch`: the current incumbent is the
working tree, the frozen evaluator and baseline remain in the run directory, and
completed attempts are recorded in one compact experiment ledger. The session is
given their paths and reads what it needs. Repository contents, diffs, evaluator
assets, command output, and run logs are not copied wholesale into its initial
context.

Do not carry forward earlier agents' messages, reasoning, tool transcripts,
compaction summaries, or narrative handoffs. Detailed evidence remains on disk
for targeted inspection. The evaluator session starts with the operator's task,
normal harness instructions, frozen invocation-time repository instructions,
evaluator-creation prompt, and evaluator documentation route. A working session
starts with the operator's task, normal harness instructions, the same frozen
repository instructions, working-session prompt, and bounded locations of the
incumbent evidence, frozen evaluator, and experiment ledger.

Preserve authority and provenance while constructing those requests. The normal
bettercodex instructions and phase prompt remain harness-owned developer
instructions; operator items remain ordered user messages; repository
instructions and selected skill bodies retain their existing lower-authority
representation; and evaluator files, ledger rows, diffs, command output, and web
material remain bounded file or tool evidence. Do not concatenate repository or
model-written text into the phase prompt. Trigger-consumed metadata prevents
replay from entering the invocation parser again without altering the original
user text.

### Operator-visible progress and result

Evaluator and working sessions are internal iterations of one loop invocation.
Their prose and tool chatter are not replayed into the operator's conversation
or the next worker.

#### Active loop status line

While a loop is active, reserve exactly one row directly below the composer and
immediately above the existing model, repository, branch, and context footer.
The existing activity row above the composer continues to show what the current
agent is doing. The loop row shows where the whole run stands.

The full row is:

```text
<name> │ <phase> │ <loop diff> │ <pulse>
```

Keep these examples in the product contract:

```text
Quality loop  │ eval │ building evaluator
Shopify speed │ 1/3  │ +0 −0       │ exploring
Shopify speed │ 1/3  │ +233 −199   │ validating
Shopify speed │ 1/3  │ +233 −199   │ kept · 18% faster
Shopify speed │ 2/3  │ +251 −204   │ promising
Shopify speed │ 2/3  │ +233 −199   │ restored · regression
Shopify speed │ 3/3  │ +240 −211   │ kept · simpler
```

The evaluator chooses a concrete one- or two-word name from the task's subject
and target, such as `Shopify speed`, `Parser cleanup`, `Startup memory`, or
`Build size`. Display `Quality loop` until evaluator setup produces that name.
Freeze it in run metadata for the rest of the invocation. It is a display label,
not the unique run identifier or directory name.

Normalize every evaluator-derived display fragment before rendering it in the
TUI or writing it to stderr. The loop name is at most two whitespace-separated
words and 32 display cells; names and decisive summaries are single-line,
control-free, free of the reserved `│` separator, and display-width bounded. If
normalization leaves no valid name, use `Quality loop`. Never write raw model
text or terminal escape bytes into operator-visible progress.

Display `eval` during evaluator construction and baselining, then `1/N` through
`N/N` for working iterations. The diff is the cumulative loop-owned text diff
against the run's starting state, including the active provisional candidate but
excluding pre-existing operator changes and `.bcodex/loops/`. If a candidate is
discarded, the numbers snap back to the restored incumbent.

The pulse is brief and evidence-backed, not free-form agent mood. Use bounded
states such as `building evaluator`, `baselining`, `exploring`, `promising`,
`validating`, `kept`, `restored`, `crashed`, and `blocked`. When the frozen
evaluator supplies one decisive result, append it concisely: `18% faster`, `8/8
checks`, `smaller bundle`, `simpler`, or the relevant failure. The harness owns
the phase words; a working session cannot stream arbitrary prose into this row.

Render the entire row as slightly dim monochrome graphite on the normal terminal
background: ANSI indexed gray `245` for the name, `243` for fields, and `240`
for separators. Do not use bold or another emphasis modifier. No part of the row
is white and no field uses cyan, green, red, yellow, or another semantic color.
Words and `+`/`−` signs carry the meaning.

On narrow terminals, omit the diff first, truncate the name by display width,
and preserve the iteration and pulse. The final fallback is:

```text
Shopify… │ 2/3 │ validating
```

If even that does not fit, omit the name, then truncate the pulse by display
width without splitting a grapheme. The irreducible active indicator is the
phase alone, such as `2/3` or `eval`. The loop row never wraps or clips into the
footer.

The row exists only while the loop is active. It disappears when the final
result is committed to the transcript, restoring the ordinary composer/footer
layout.

#### Non-interactive progress

For a CLI prompt without the TUI, keep stdout's existing single-final-answer
contract. In line mode, preserve the existing prompts and emit only the loop's
final answer for that submission. Write sparse loop progress to stderr at major
phase changes: evaluator construction, baseline completion, each iteration
start, each resolved verdict, recovery, and a terminal blocker. Reuse the same
name, `eval` or `I/N` phase, bounded pulse, and concise decisive result as the
TUI row, without ANSI styling, cumulative diff output, worker prose, or tool
chatter. Flush each line so a long loop does not look hung when stderr is being
observed.

Each stderr progress line has this stable shape:

```text
<name> │ <phase> │ <pulse>
```

#### Final result

At the end, bettercodex returns one result containing the run identifier and
path, evaluator-setup status, baseline and final evaluator results when
available, counts of kept, discarded, crashed, blocked, and interrupted
iterations, the final repository state, evaluator blind spots, and anything
left unvalidated. Kept changes remain in the operator's worktree.

## Reference design: autoresearch

Do not invent a miniature multi-agent framework for a problem that is already
better expressed as a simple loop. The reference design is Andrej Karpathy's
[`autoresearch`](https://github.com/karpathy/autoresearch). This specification
was checked against
[`program.md` at `228791fb499afffb54b46200aca536f79142f117`](https://github.com/karpathy/autoresearch/blob/228791fb499afffb54b46200aca536f79142f117/program.md).

`autoresearch` is deliberately tiny. A Markdown program tells an ordinary agent
what it may change, what it may not change, how to evaluate the result, when to
keep or discard a change, and then to repeat. The repository and Git state carry
the result forward. Its `program.md` is, in Karpathy's own description,
"essentially a super lightweight skill."

The port is direct:

| `autoresearch` | bettercodex quality loop |
| --- | --- |
| `program.md` | the bundled `$loop` program and its two exact prompts |
| fixed `prepare.py` evaluator | the task-specific evaluator built once, baselined, then frozen |
| fixed time budget and `val_bpb` | frozen task-specific inputs, budgets, acceptance rule, and comparison result |
| editable `train.py` | the evaluator-declared candidate boundary in the operator's worktree |
| current branch commit | the current incumbent snapshot |
| one experimental idea and commit | one coherent hypothesis and provisional candidate |
| `run.log` | one iteration evidence directory |
| `results.tsv` | repository-local `results.tsv` |
| keep the commit or reset | keep the candidate or restore the incumbent |
| repeat forever | repeat for the operator's exact finite count |

The details around that table matter. `autoresearch` establishes the baseline
before changing behavior, fixes the evaluator and budget, changes one bounded
surface, tests one idea at a time, writes large output to `run.log`, reads only
the decisive lines or a crash tail, records every attempt compactly, advances
only on evidence, and resets failures. Those are the principles to port; the
words `KEEP` and `DISCARD` alone are not the design.

The deliberate adaptations are the ones already required by this product: the
evaluator is generated from an arbitrary task instead of being hard-coded for
one training script; ordinary dirty worktrees are protected instead of requiring
a fresh experiment branch; the valid trigger itself replaces the setup
confirmation; the loop has an exact operator-selected finite count; and each
iteration gets a fresh context instead of one indefinitely growing agent
context. Generating the ruler creates more risk than `autoresearch`'s fixed
`prepare.py`, which is why clean baseline reproduction, an explicit integrity
set, and grader-hacking review are part of the port. These adaptations preserve
the loop's control structure rather than replacing it.

The `$loop` skill is bettercodex's equivalent of `program.md`. The `/loop`
command is only a convenient second way to invoke that same program.

This also confirms why the harness must own repetition. Karpathy
[reported that Codex ignored `NEVER STOP`](https://github.com/karpathy/autoresearch/issues/57)
and explicitly pointed toward a `/loop` mechanism. A stronger paragraph alone
does not create a reliable loop.

## Non-goals

This specification does not include codebase compartmentalization,
per-compartment manifests, startup manifest checks, or hooks that block access
to labeled parts of a repository. Those ideas were deliberately put aside for a
later design.

It also does not add:

- an always-on loop for ordinary tasks;
- multiple agents running together, reviewer/editor roles, or agent-to-agent
  communication;
- a general workflow engine, plugin system, hook framework, or configuration
  framework;
- a universal quality score or hosted evaluation service;
- a self-modifying evaluator or a worker that rewrites its own loop program;
- an indefinite background research mode; or
- another model, reasoning setting, provider, or global store for run artifacts.

## Implementation shape

The loop program is a bundled, explicit-only system skill, but its invocation is
harness control syntax rather than an ordinary model-selected skill turn. The
harness resolves and consumes `$loop` before generic skill mention injection.
`loop` is a reserved built-in name and cannot be shadowed, disabled, or made
implicit by a repository or user skill. `/loop` and `$loop` both construct the
same typed loop request and enter the same engine; neither sends the task through
the current general agent first.

Keep the orchestration in one focused loop module with one durable state machine.
Reuse bettercodex's existing agent, tool, repository-instruction, approval,
transport, and TUI behavior for each fresh session. The only special session
behavior is the clean initial context, repository-local persistence, and the
fixed evaluator/working prompt selected by the current phase.

The exact prompts and evaluator-contract schema in this specification have one
bundled source of truth and are not copied into `prompts/system.md`. Adding the
feature must not restore the removed engineering-ownership block, introduce a
second agent runtime, or create a parallel implementation of retained Codex
behavior.

The installed system skill must make the evaluator routes from
[`docs/evals/MANIFEST.md`](docs/evals/MANIFEST.md) available in every target
repository. That source file remains the source of truth; target repositories do
not need to contain or commit a copy.

The inline `$loop` trigger works in interactive and non-interactive prompts.
`/loop` is the interactive command form. When the loop is not invoked,
bettercodex follows its ordinary single-session path and creates no loop state.

## Acceptance criteria

The feature is ready only when all of the following are demonstrated.

### Invocation and orchestration

- `$loop` works at the beginning, middle, and end of a task, and `/loop` reaches
  the same typed loop request in interactive input. `$loop` also works in
  non-interactive input.
- `/loop` appears in the slash popup, completes as a task prefix, preserves
  attachments, and enters the loop only when the completed task is submitted.
  If another ordinary task is active, it follows normal next-task queueing and
  does not splice itself into that turn.
- No explicit count produces three working sessions. `$loop 5 times`, `$loop 5
  iterations`, `$loop 5x`, and `5x $loop` produce five; `/loop 8x <task>`
  produces eight. Each is preceded by exactly one evaluator session.
- Parser cases prove that unrelated task numerals are preserved, repeated
  agreeing triggers create one run, one explicit count wins over repeated
  triggers with no count, and invalid, zero, fractional, signed, overflowing,
  malformed, missing-task, and conflicting invocations start no sessions and
  leave no run directory. Bare task integers and count words separated from the
  trigger by a task word remain task content.
- Near matches such as `$looper` and `/looping` remain ordinary task text; an
  exact `$loop` still triggers when it appears inside quoted or fenced task text,
  because the product contract says anywhere in the submission.
- A task attachment satisfies the non-empty-task rule and is preserved. A
  trigger-only submission without task text or attachments is rejected.
  Attachment bytes are not run through OCR or searched for loop syntax;
  `$loop` visible only inside an image does not trigger the feature.
- Trigger resolution happens before generic skill injection. A repository or
  user skill named `loop` cannot shadow the built-in, and replaying the original
  `$loop` text cannot recursively invoke it.
- An ordinary prompt starts no evaluator, creates no `.bcodex/loops/` state, and
  follows the existing agent path.
- Distinct working iterations have distinct fresh session identities and run
  sequentially over the incumbent left by the last `KEEP`.
- The harness starts the full requested count even when an earlier agent says it
  is finished, makes no change, returns `DISCARD`, or crashes.
- Setup and worker completion accept only the fixed four-line envelopes and
  valid run-relative evidence paths. Missing fields, extra verdicts, traversal,
  stale-state evidence, or a verdict mentioned only in surrounding prose cannot
  mutate loop state.
- A supported `BLOCKED` reason stops the loop; `BLOCKED` used as "done" is
  recorded as malformed and does not skip remaining iterations. Only an
  accepted blocker, evaluator failure or tampering, state-integrity failure, an
  operator interrupt, or count exhaustion stops the loop.

### Evaluator quality

- The single evaluator session runs before the first working session and writes
  only inside its provided evaluator workspace. Staged, unstaged, untracked,
  ignored, Git, other run-state, and generated changes made elsewhere during
  setup are restored before validation.
- The package has a schema-valid machine-readable contract, runnable checks,
  bounded outputs, a human-readable rationale, explicit candidate and integrity
  boundaries, acceptance and comparison rules, and declared blind spots.
- Oversized packages, path or argument fields, check counts, fixtures, and
  extracted results fail setup explicitly; no protected content or decisive
  case is silently truncated or omitted.
- Contract validation rejects candidate overlap with the integrity set or
  protected run control state, and rejects scratch overlap with the candidate or
  integrity set. A repository oracle that the task may edit is not accepted as
  a frozen gate under a broad path pattern.
- A side-effect fixture proves that an external mutation cannot be a provisional
  candidate or automatic gate. A task that indispensably requires a deployment,
  purchase, message, or other non-restorable repeated action blocks before the
  loop performs it.
- The harness, not the evaluator session's prose, reproduces every
  machine-checkable starting-state result after restoration and writes row `0`
  in `results.tsv`. Model-judged baseline output is a separate structured
  artifact bound to the same state identity. A package that depended on a
  restored file, fails machine reproduction, or cannot detect its claimed
  failure is rejected.
- Focused fixtures cover a behavioral promise, a noisy performance comparison,
  a binary task with no defensible secondary score, a genuinely subjective
  artifact, and a task whose real outcome cannot be evaluated reliably. They
  prove task-specific promise selection rather than a canned quality checklist.
- Grader-hacking cases prove that changing a protected fixture, runner, schema,
  rubric, command, expected result, timeout, or oracle-bearing repository asset
  cannot produce `KEEP`. Rewriting the baseline, ledger, or earlier evidence is
  also detected. A mutable existing test that is not in the integrity set cannot
  be the sole acceptance gate.
- Model-judged gates require the frozen rubric and structured artifact evidence.
  Tests cover calibrated pass/fail or pairwise use, missing evidence, position
  order where relevant, and an uncalibrated opinion being advisory or blocking
  setup rather than silently becoming a hard gate.
- Measurement cases prove that fixed inputs, budgets, environment identity,
  baseline repeats or tolerance, ties, noise, and inconclusive outcomes are
  applied identically to every candidate. An inconclusive candidate is never
  kept as an improvement.
- Modifying the protected evaluator path set, file type, mode, bytes, or symlink
  target during a working iteration is detected before `KEEP` and stops the run
  without retaining the candidate.

### Repository state and evidence

- A fixture with staged, unstaged, deleted, untracked, and ignored pre-existing
  files survives `DISCARD`, crash, interruption, and cold recovery exactly.
- A valid `KEEP` leaves the complete candidate, including created and deleted
  files and permitted Git-state changes, as the next incumbent. A no-change
  `KEEP` becomes `discard`.
- Canonical state identities change for relevant `HEAD`, branch, index, path
  type, mode, symlink-target, or byte changes and remain stable across mtimes and
  run-directory changes.
- Declared scratch paths are disjoint from candidate and integrity paths. Test
  caches, generated files, logs, and other evaluator side effects from both the
  worker's runs and harness validation are restored before `KEEP`. The recorded
  candidate identity still matches the post-validation worktree.
- Pre-existing ignored data survives every outcome. Newly created or modified
  ignored caches are removed or restored before identity capture unless the
  frozen boundary explicitly names them as candidate work.
- Unsupported mutation of another ref, stash, remote, repository config, hook,
  linked worktree, or submodule repository blocks safely. A task that explicitly
  needs one of those surfaces succeeds only when its evaluator declares and
  proves exact snapshot and restoration support.
- Every attempt has one ledger row and one evidence directory. The ledger stays
  parseable, append-only, and compact while detailed output remains available by
  its relative evidence path. Escaped tabs, newlines, and backslashes round-trip.
- `.bcodex/loops/` is ignored without changing tracked `.gitignore` content, and
  repeated setup does not duplicate the exclusion. Tracked, symlinked, or unsafe
  loop-state paths are rejected before a session starts.
- Run artifacts use private permissions, remain after completion, and are not
  duplicated into global bettercodex storage. Evaluator and worker sessions do
  not appear as independent resumable sessions.
- Concurrent loop invocation in one worktree is rejected without disturbing the
  active run.
- Crash recovery restores only when the durable incumbent still matches the
  worktree evidence. A post-crash external edit produces a visible blocked
  conflict and is never overwritten.
- Recovery fixtures interrupt every ledger-publication boundary, reject an
  incompatible protocol, prompt, model, or contract identity, remove only torn
  trailing TSV data, and never duplicate or rewrite a complete row.
- External edits during evaluator setup or an active iteration are never folded
  into a candidate. Attributable loop changes are recovered, ambiguous paths are
  left untouched with conflict evidence, and the run blocks before baseline,
  `KEEP`, restoration over that path, or another worker.
- Foreground timeout, background-process, crash, and interrupt fixtures prove
  that all phase-owned process trees are terminated and reaped before
  restoration or the next worker. Pre-existing operator processes survive.

### Active loop status line

- Rendered TUI tests place exactly one loop row between the composer and existing
  footer while a run is active, without moving the activity row from above the
  composer.
- Evaluator setup displays `Quality loop │ eval`, then adopts and freezes the
  evaluator's one- or two-word name. Working sessions display the current
  `iteration/total` value.
- The displayed additions and deletions include only cumulative loop-owned text
  changes. Tests cover a dirty starting tree, a provisional candidate, `KEEP`,
  and the numbers snapping back after `DISCARD`.
- The pulse uses only bounded harness phases and concise frozen-evaluator
  results. Worker prose cannot enter the row. Control characters, the reserved
  separator, overlong labels, and terminal escape bytes in evaluator output are
  normalized or replaced before display.
- Buffer-style assertions prove that the name uses indexed gray `245`, fields
  use `243`, separators use `240`, and no cell uses white, a semantic color, or
  an emphasis modifier.
- Width tests cover the full examples, omission of the diff, display-width name
  truncation, the `Shopify… │ 2/3 │ validating` fallback, name omission, pulse
  truncation, and the phase-only minimum without wrapping or clipping.
- Completing, blocking, or interrupting the loop removes the row and restores
  the unchanged inactive composer/footer layout.

### Non-interactive progress

- Prompt-argument tests keep the final result alone on stdout. Line-mode tests
  preserve their existing prompt stream and add only the loop's final result for
  that submission. Both emit unstyled, flushed stderr lines only for the
  specified major loop phase changes.
- Non-interactive progress uses the frozen loop name, `eval` or `I/N`, bounded
  pulse vocabulary, and decisive result. It never exposes internal model prose,
  tools, evidence logs, or the TUI-only cumulative diff.

### Context and operator control

- Captured Responses requests prove that every evaluator and working session
  receives the task-defining operator messages verbatim, including attachments
  and order.
- Attachment payload, detail, and placement remain identical after the original
  source file is changed or removed; an attachment that cannot be durably
  replayed causes invocation to fail before setup.
- Earlier and later operator inputs preserve their same-authority order, while
  synthetic repository messages, assistant turns, tool output, and compaction
  summaries do not enter the frozen task record. An oversized verbatim record
  is rejected rather than summarized.
- Captured requests also prove that the phase prompt retains developer
  authority, operator text retains user authority, and repository instructions,
  skills, evaluator artifacts, ledger content, diffs, and tool output are not
  promoted by string concatenation.
- A candidate that edits `AGENTS.md`, another applicable instruction file, or an
  explicitly selected skill does not change the frozen instruction items seen by
  later workers in the same run. A new run discovers the retained files normally.
- Working sessions receive no previous agent messages, reasoning, tool calls,
  tool output, compaction summaries, or prose handoff. They receive only normal
  instructions, the fixed working prompt, and bounded paths to repository-local
  evidence.
- A missing original task item or attachment fails before setup. A task whose
  only meaning is an excluded assistant proposal reports the missing contract
  instead of treating that proposal or a compaction summary as user intent.
- Task messages entered while the loop runs do not alter the frozen task or
  evaluator. An interrupt restores the active incumbent and prevents another
  worker from starting.
- The final operator result identifies the run directory, baseline and final
  results, every outcome count, retained repository state, blind spots, and
  unvalidated behavior.

### Behavioral evidence

Deterministic orchestration tests use scripted evaluator and worker responses to
exercise setup restoration, baseline, keep, harness-overridden keep, discard,
crash, valid and invalid blocked results, tampering, interruption, validation
side effects, recovery conflict, dirty-worktree preservation, and exact-count
paths. Tests inspect the resulting model requests, filesystem, Git state, state
identities, ledger, evidence package, and global saved-session list; testing only
count parsing, evaluator schema parsing, or a state-machine helper is
insufficient.

Before release, run matched live `gpt-5.6-sol` evaluations from identical
repository states and with the same `max` reasoning effort. Build the task set
from representative bettercodex work, concrete historical failures, and the
actual intended distribution. Include tasks decided by behavior and by a
measurable optimization property, plus tasks where footprint, usability, or
maintainability genuinely determines quality; these are coverage strata, not a
canned rubric applied to every task.

Preregister each task's objective, cases, hard acceptance gates, pairwise or
metric comparison rule, blind spots, repetition count, and aggregate release
threshold before running either arm. Compare one ordinary session with the
default loop, randomize arm order, preserve every trial, and use enough repeated
runs to expose model and measurement variance. Any subjective final comparison
must be blinded where possible and calibrated against trusted operator or expert
labels; do not let the loop grade its own release evaluation.

Do not collapse unrelated tasks into one vague quality score. Report per-task
hard-pass rate, pairwise wins/ties/losses or defensible metric deltas, evaluator
setup failures, candidates retained despite regressions, tokens, latency, and
cost. Do not accept the feature without the preregistered positive quality
signal, zero candidates kept despite failing hard checks, exact restoration in
all destructive-path fixtures, and no unexplained regression in ordinary
non-loop coding tasks. The evaluator session and extra working sessions are
expected cost, not evidence of quality by themselves.

## Upstream discipline

For retained Codex behavior, implementation work must inspect and port the
current upstream source. At the time this direction was established, the
inspected upstream revision was
[`572954683910555cbbe3034bc8a2a0aa2bc7e66a`](https://github.com/openai/codex/tree/572954683910555cbbe3034bc8a2a0aa2bc7e66a).
This does not force bettercodex to import Codex's configuration framework,
plugin system, alternate models, or unrelated product surface.

## Terms

- **Loop run:** one invocation, containing evaluator setup and the requested
  number of working iterations.
- **Evaluator session:** the first fresh session, dedicated only to constructing
  and baselining the evaluation package.
- **Working session:** one fresh session that implements, reviews, edits, and
  evaluates the repository in a single sequential iteration.
- **Incumbent:** the exact repository state retained at the start of an
  iteration.
- **Candidate:** the provisional state produced by that iteration before the
  harness keeps or restores it.
- **Candidate boundary:** the task-specific repository and Git surfaces a worker
  may change without altering the evaluator or inherited constraints.
- **State identity:** the canonical digest of the protected repository and Git
  state used to prove which baseline, incumbent, or candidate was evaluated.
- **Hypothesis:** the one coherent implementation idea an iteration tests; it
  may require several coupled edits without becoming unrelated cleanup.
- **Loop name:** the frozen one- or two-word display label chosen during
  evaluator setup; it is not the run identifier.
- **Pulse:** the bounded, evidence-backed final field in the active loop status
  line.
- **Evaluation package:** the machine-readable contract and small set of
  runnable promise-level checks, protected assets, baseline evidence, acceptance
  rule, comparison rule, and declared blind spots constructed before
  implementation.
- **Integrity set:** the frozen evaluator paths and oracle-bearing repository
  assets whose types, modes, bytes, and targets give the ruler its meaning.
- **Experiment ledger:** the run's compact, append-only `results.tsv` index over
  the baseline and every attempted candidate.
