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
