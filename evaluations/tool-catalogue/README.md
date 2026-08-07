# Tool catalogue evaluation

This is a historical, focused diagnostic, not a general capability or release
evaluation. Both arms are bettercodex binaries, many prompts explicitly name
the required tool, the run has only two repetitions, and both arms reached the
24/24 ceiling. It supports the narrow claim that the concise catalogue did not
break these cases; it does not establish parity with Codex CLI or absence of
model degradation. See `evaluations/README.md` for the current standard.

`2026-08-05-matched-ab.json` is the retained output of the frozen
matched A/B run used to evaluate the concise tool catalogue. It contains every
case, repetition, hard-grade check, tool call, model response, usage count, and
duration recorded within that runner's output bounds.

Run command:

```sh
scripts/evaluate_tool_catalogue.py \
  --arm baseline=/tmp/bettercodex-tool-eval-baseline-bcodex \
  --arm candidate=/tmp/bettercodex-tool-eval-candidate-bcodex \
  --repetitions 2 \
  --output /tmp/bettercodex-tool-catalogue-ab-results.json
```

Frozen inputs and output:

| Artifact | SHA-256 |
| --- | --- |
| Evaluation script used by the run | `877df344eb43ea94fa80df1a279c65e98714b1c237edb9e5702abc2833f568de` |
| Baseline binary | `a40ca9ed99795063a157608fca2f9fbf30586b409dc59bcbf252e864b6309b68` |
| Baseline catalogue | `bab81c3d1d5295c745067e98923acecdb4389739be78e448df2cefd495ea97aa` |
| Candidate binary | `9f87045a972a81748eff2a41b519265f0c58845e8dd43ec9e1acbb367a22c0d0` |
| Candidate catalogue | `bd15d871df1356afdec9bd28459626a5042e7e780cc907ad6b4e402f4e90f112` |
| Complete result JSON | `a7c5e790a4bdbc3a2ba5eb3cf33e8d802dfa5b6c9febbbb649858e8757d759cb` |

The previous catalogue is the `prompts/tool-catalogue.md` blob at commit
`33d7e73661`; the concise catalogue is the blob at `7f1c5fec0c`. Their hashes
match the frozen catalogue hashes above.

The historical protocol, acceptance rule, aggregate summary, and all 48
randomized run records are embedded in the JSON. `prompts/tool-context.md`
records the original interpretation and limitations.
