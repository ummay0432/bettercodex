# Tool catalogue evaluation

These are focused diagnostics, not general capability or release evaluations.
Both arms are bettercodex binaries, many prompts explicitly name the required
tool, each run has only two repetitions, and every retained arm reached the
24/24 ceiling. They support only the narrow claim that the catalogues did not
break these cases; they do not establish parity with Codex CLI or absence of
model degradation. See `evaluations/README.md` for the current standard.

## 2026-08-07 single-block renderer

`2026-08-07-single-block-ab.json` records the fresh run for the fixed-surface,
single-declaration renderer. The checked-in evaluation script was used
unchanged. All 12 cases tied 2/2, so its predeclared extra-repetition rule did
not trigger.

| Recorded measure | Previous catalogue | Single-block catalogue | Change |
| --- | ---: | ---: | ---: |
| Hard-graded passes | 24/24 | 24/24 | tied |
| Complete catalogue `o200k` estimate | 2,964 | 1,379 | -53.5% |
| Aggregate backend input tokens | 509,831 | 352,612 | -30.8% |
| Aggregate output tokens | 13,686 | 12,901 | -5.7% |
| Aggregate reasoning-output tokens | 6,095 | 6,014 | -1.3% |
| Aggregate wall time | 468.130 s | 420.131 s | -10.3% |

The aggregate differences are observations from this randomized low-sample run,
not performance claims. The artifact retains every result, including usage and
duration. Its SHA-256 is
`8b68a6fda4d62b15e650d779ef3bfed41218802c98703a6fcf7ee6e2320e1dc5`.

| Frozen input | SHA-256 |
| --- | --- |
| Evaluation script | `877df344eb43ea94fa80df1a279c65e98714b1c237edb9e5702abc2833f568de` |
| Baseline binary | `76384e55f5f5a0ee99bcd37f2c6716b8b3462eac70f4e9406b5d441f81ddaa15` |
| Baseline catalogue | `d915e74c9626b6be3d3233afcbedfb0d9ccda8c37d2dd5a0f4d86e231729ed70` |
| Candidate binary | `a9b84fa443f2a7c1515556111d0d87e2824dd69c93a3bd4866fac2fde0189e40` |
| Candidate catalogue | `3d06d50c5fb7d552dcf10d5daf10711d39fddc74c907400b0392d9033faeba4f` |

Run command:

```sh
scripts/evaluate_tool_catalogue.py \
  --arm baseline=/path/to/pre-change/bcodex \
  --arm candidate=target/release/bcodex \
  --repetitions 2 \
  --output /tmp/bettercodex-tool-catalogue-ab-results.json
```

## 2026-08-05 concise catalogue

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
