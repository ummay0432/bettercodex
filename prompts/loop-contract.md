# Harness-owned loop paths and evaluator contract

Run root: `{{run_root}}`
Candidate worktree root: `{{worktree_root}}`
Evaluator workspace: `{{evaluator_workspace}}`
Evaluation docs manifest: `{{eval_manifest}}`

Write `contract.json` inside the evaluator workspace. The harness accepts this
bounded version-1 shape (unknown fields are rejected):

```json
{
  "version": 1,
  "loop_name": "one or two words",
  "promises": [{
    "id": "stable-id",
    "class": "acceptance|improvement|regression",
    "statement": "observable promise",
    "failure_mode": "specific failure caught",
    "method": "machine check or frozen model rubric",
    "required_evidence": ["artifact or result"]
  }],
  "candidate_paths": ["normalized/repository/path", "prefix/**"],
  "fixed_constraints": ["inherited constraint"],
  "integrity_paths": ["normalized/repository/oracle/path"],
  "scratch_paths": ["normalized/repository/scratch/path"],
  "machine_checks": [{
    "id": "stable-check-id",
    "promise_ids": ["stable-id"],
    "argv": ["program", "literal-argument"],
    "cwd": ".",
    "env": {},
    "input_paths": [{"root": "worktree", "path": "candidate/path"}],
    "fixture_paths": [{"root": "evaluator", "path": "evaluator/workspace/fixture"}],
    "timeout_seconds": 300,
    "resource_budget": "bounded local budget",
    "side_effects": "none|declared_scratch",
    "approval": "none",
    "expected_exit_codes": [0],
    "extract": {"kind": "pass|last_line|json_number", "json_pointer": "/optional"},
    "baseline_repeats": 1
  }],
  "discrimination_checks": [{
    "linked_check_id": "stable-check-id",
    "check": {
      "id": "stable-known-failure-id",
      "promise_ids": ["stable-id"],
      "argv": ["same-runner", "known-failure-arguments"],
      "cwd": ".",
      "env": {},
      "input_paths": [{"root": "worktree", "path": "candidate/path"}],
      "fixture_paths": [{
        "root": "evaluator",
        "path": "evaluator/workspace/known-failure-fixture"
      }],
      "timeout_seconds": 300,
      "resource_budget": "bounded local budget",
      "side_effects": "none|declared_scratch",
      "approval": "none",
      "expected_exit_codes": [0],
      "extract": {"kind": "pass|last_line|json_number", "json_pointer": "/optional"},
      "baseline_repeats": 1
    }
  }],
  "model_checks": [{
    "id": "stable-check-id",
    "promise_ids": ["stable-id"],
    "kind": "pass_fail|pairwise",
    "rubric_path": "evaluator/workspace/rubric.md",
    "required_artifacts": ["artifact description"],
    "calibration_paths": [],
    "output_shape": "documented JSON shape",
    "hard_gate": false
  }],
  "acceptance": {"required_check_ids": ["stable-check-id"]},
  "comparison": {
    "kind": "acceptance_transition|metric|pairwise",
    "check_id": null,
    "direction": null,
    "minimum_delta": 0.0,
    "tolerance": 0.0,
    "ties": "discard",
    "inconclusive": "discard"
  },
  "environment": ["identity or assumption"],
  "uncovered": ["blind spot"],
  "known_loopholes": ["known evaluator limitation"]
}
```

Paths are relative to the named root, may not be absolute, contain `..`, or
traverse symlinks. `candidate_paths`, `integrity_paths`, and `scratch_paths`
must be pairwise disjoint. A trailing `/**` means that normalized directory and
its descendants. The whole-root path `.` is not a valid candidate boundary
because it includes harness-owned `.bcodex/loops/`; enumerate the actual editable
files or disjoint trees. Machine checks are argument vectors, never shell strings;
their working directories are relative to the candidate root. Automatic checks
must enumerate at least one input. Worktree inputs must be inside the candidate
or integrity boundary, and every worktree fixture must be protected by the
integrity boundary. Evaluator inputs and fixtures use run-relative paths under
`evaluator/workspace/`. Automatic checks must use `approval: "none"` and may
write only declared scratch paths. Use
`json_number` only for a finite numeric metric. `acceptance_transition` keeps a
candidate only when it changes an unacceptable incumbent into an acceptable
candidate. `metric` additionally requires the named check to improve in the
declared `higher` or `lower` direction by more than tolerance and at least the
minimum delta. `pairwise` names a pairwise model check with at least two frozen
calibration examples, zero numeric thresholds, both candidate-first and
incumbent-first judgments, and a consistent result. Ties and inconclusive
results are always discarded.

Every machine check used by acceptance or metric comparison needs at least one
`discrimination_checks` entry. Its nested check must cover the same promises,
run the same program, execute exactly once, and name a frozen evaluator fixture
representing the linked check's stated failure. The harness runs these probes
against the restored starting state and rejects setup unless all probes prove
that the runner catches their known failures.

The setup baseline file must be a bounded JSON object with this control shape:
`{"state":"<digest from {{starting_state_file}}>","checks":{"<check-id>":
{"passed":true,"artifacts":[]}}}`. Name every machine and model check. Record
repeats, values, or variance in additional fields inside each check object.
Machine `passed` claims must agree with the harness reproduction. Every model
check supplies a boolean `passed` and concrete string `artifacts` covering its
declared evidence, plus any calibration result needed by its frozen rubric.
For a `pairwise` model check, also supply `calibration_passed: true`, exact
`orders: ["candidate_first", "incumbent_first"]`, and `consistent: true`; the
harness will not accept a better judgment without both position orders.
For a `BLOCKED` worker verdict, the artifact also needs a `blocker` object with
exact fields `kind`, `detail`, and non-empty `evidence`. `kind` is one of
`task_evaluator_contradiction`, `missing_prerequisite`, `missing_authority`,
`state_integrity`, or `external_conflict`; completion or lack of a better idea
is not a blocker.
Also write a concise `RATIONALE.md` in the evaluator workspace. The harness
restores all production changes before reproducing machine results itself; do
not rely on setup-session changes outside this workspace.
