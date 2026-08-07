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
    "timeout_seconds": 300,
    "resource_budget": "bounded local budget",
    "side_effects": "none|declared_scratch",
    "approval": "none",
    "expected_exit_codes": [0],
    "extract": {"kind": "pass|last_line|json_number", "json_pointer": "/optional"},
    "baseline_repeats": 1
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
    "kind": "acceptance_transition|metric",
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
must use `approval: "none"` and may write only declared scratch paths. Use
`json_number` only for a finite numeric metric. `acceptance_transition` keeps a
candidate only when it changes an unacceptable incumbent into an acceptable
candidate. `metric` additionally requires the named check to improve in the
declared `higher` or `lower` direction by more than tolerance and at least the
minimum delta. Ties and inconclusive results are always discarded.

The setup baseline file must be a bounded JSON object with this control shape:
`{"state":"<digest from {{starting_state_file}}>","checks":{"<check-id>":
{"passed":true,"artifacts":[]}}}`. Name every machine and model check. Record
repeats, values, or variance in additional fields inside each check object.
Machine `passed` claims must agree with the harness reproduction. Every model
check supplies a boolean `passed` and concrete string `artifacts` covering its
declared evidence, plus any calibration result needed by its frozen rubric.
Also write a concise `RATIONALE.md` in the evaluator workspace. The harness
restores all production changes before reproducing machine results itself; do
not rely on setup-session changes outside this workspace.
