# Behavioral evaluation policy

Behavioral evaluations make live model calls. They are useful evidence, but
they are not trustworthy merely because a JSON file calls itself an eval. A
release conclusion requires a representative corpus, hidden deterministic
graders, calibrated fixtures, repeated matched runs, and complete records.

## Audit result

The public OpenAI Codex repository has extensive deterministic tests but no
public end-to-end model-evaluation corpus or release runner to port. The
bettercodex records in this directory therefore remain focused observations:

- `tool-catalogue/` compares two bettercodex catalogue variants on prompts that
  often name the expected tool. It is a useful focused diagnostic, but it does
  not compare against Codex CLI or measure general coding capability.
- `compaction/` records one successful live nonce-recall run per arm. Its runner
  was not retained and opaque server payloads were omitted, so the record is
  not independently reproducible and cannot be a release gate.

Neither artifact should be cited as proof that bettercodex has no model
degradation.

## Matched runner

`scripts/evaluate_harness.py` runs explicit bettercodex and Codex CLI arms with
the same fixture, prompt, account, `gpt-5.6-sol` model, maximum reasoning, full
user permissions, timeout, repetition count, and shuffled matched schedule.
Codex runs ephemerally with user configuration and local execution rules
disabled. bettercodex gets a fresh state directory. The shipped harness prompts
and tool interfaces are left intact because those are what the comparison is
intended to measure.

Before any model call, every case is calibrated twice:

1. Its initial fixture must fail at least one grader check.
2. Its held-out oracle patch must make every grader check pass.

The grader and oracle are never copied into the agent workspace. Each result
retains the binary hash and version, exact invocation, prompt and corpus hashes,
full normal-sized stdout/stderr/trace, tool and usage records, grader checks,
initial and final file manifests, and changed-file contents. Over-limit output
or any authentication-value redaction makes that protocol record fail instead
of quietly truncating evidence. The output JSON is atomically checkpointed
after every arm.

Acceptance cannot be rescued by an average. A release-eligible run requires at
least five repetitions, a complete matched matrix, no lower bettercodex pass
count overall or in any individual case, and every critical bettercodex control
passing every repetition. Paired wins and losses are reported directly; five
repetitions are still too few for a broad statistical equivalence claim.

Validate the bundled corpus without spending inference tokens:

```sh
scripts/evaluate_harness.py --list
scripts/evaluate_harness.py --dry-run --repetitions 5 --seed 20260807
```

Run a live matched diagnostic after building the candidate:

```sh
./scripts/dev.py cargo build --release
scripts/evaluate_harness.py \
  --bettercodex target/release/bcodex \
  --codex "$(command -v codex)" \
  --repetitions 5 \
  --output /tmp/bettercodex-matched-eval.json
```

The processes intentionally have the operator's filesystem permissions because
that matches bettercodex. Run only trusted corpora and graders.

## Corpus standard

`diagnostic-cases/` contains six public synthetic smoke cases: two coding
repairs with hidden randomized checks, an adversarial and benign repository
instruction pair, and an adversarial and benign tool-output pair. They detect
runner breakage and obvious harness regressions, but the runner always reports
them as `diagnostic_only`. Public synthetic tasks written alongside the harness
are too easy to tune against and are not representative production evidence.

A suite is a directory containing `suite.json` and one directory per named
case. Every case contains:

- `case.json`: prompt, family, tags, working directory, and provenance;
- `fixture/`: the only files visible to the agent;
- `grader.py`: a deterministic program that receives the workspace path and
  prints a nonempty JSON object of boolean checks; and
- `oracle.patch`: a held-out patch that must pass every check.

For a release decision, use a suite outside the repository built from
operator-authored tasks, real production incidents, or upstream regressions.
Set `release_eligible` only for that corpus. The runner additionally requires
non-synthetic provenance, at least two coding cases, and adversarial/benign
pairs for repository instructions and tool output. Keep the corpus private,
version it independently, and retain its corpus hash with the result.

This design follows OpenAI's guidance to use task-specific, production-like
data; include typical, edge, and adversarial cases; automate scoring; and run
continuous evaluation. It also reflects OpenAI's 2026 coding-eval audit, which
found that underspecification, overly strict tests, misleading prompts, and low
coverage can invalidate apparently objective tasks:

- [Evaluation best practices](https://developers.openai.com/api/docs/guides/evaluation-best-practices)
- [Separating signal from noise in coding evaluations](https://openai.com/index/separating-signal-from-noise-coding-evaluations/)
- [Instruction Hierarchy Challenge](https://openai.com/index/instruction-hierarchy-challenge/)

The current public diagnostics do not cover long multi-turn sessions, resume,
automatic or manual compaction, skills, or live web output. Any change to those
paths still needs a calibrated matched suite for the affected lifecycle; the
historical compaction observation does not fill that gap.
