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
