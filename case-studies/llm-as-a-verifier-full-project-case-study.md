# Case study: LLM-as-a-Verifier — project anatomy, evidence, and bettercodex transfer analysis

> **Reporting cutoff:** August 18, 2026\
> **Repository:** [https://github.com/llm-as-a-verifier/llm-as-a-verifier][repo]\
> **Source snapshot inspected:** [`115de305f23ed89bc42e86e010853c40059f3f7d`][snapshot], committed August 14, 2026\
> **Paper:** [*LLM-as-a-Verifier: A General-Purpose Verification Framework*][paper], arXiv `2607.05391v2`\
> **Python package:** [`llm-verifier` 0.2.0][pypi]\
> **License:** MIT

## Status and editorial scope

This document is a repository-wide technical case study of LLM-as-a-Verifier. It is intended to give another agent enough context to understand the project's motivation, mathematics, implementation, public API, backend behavior, prompt design, tournament algorithm, progress estimators, datasets, benchmark methodology, research claims, reproducibility boundaries, operational risks, and relevance to bettercodex without first reading the entire upstream repository.

Four evidence classes are kept separate throughout:

1. **Paper claims** come from the July 2026 paper and include experiments whose implementation is not necessarily present in the repository.
2. **README and changelog claims** describe the maintainers' intended product behavior and reported results.
3. **Current source behavior** refers to direct inspection of the pinned August 14 commit.
4. **Local audit findings** come from deterministic inspection of the shipped code, package artifact, and datasets. No paid verifier-model benchmark was rerun for this case study.

That distinction matters. The paper presents a broad verification framework spanning test-time selection, progress estimation, reward modeling, robotics, and reinforcement learning. The public repository is narrower: a compact Python scoring library, benchmark adapters, criteria files, reproduction scripts, and a large collection of previously generated trajectories. It is not an agent harness, candidate generator, terminal environment, objective grader, reinforcement-learning system, or full reproduction package for every paper experiment.

No bettercodex runtime, prompt, tool schema, model-visible error, or product behavior was changed as part of this study.

## Executive verdict

**Yes: several ideas are strong candidates for bettercodex, but the project should be treated as a source of verification principles rather than code to port wholesale.**

The most transferable ideas are:

1. judge observable task evidence rather than the agent's declaration of success;
2. compare candidate actions or answers pairwise instead of asking for an uncalibrated absolute confidence score;
3. split verification into narrow, independently scored criteria;
4. use the expected value of score-token probabilities rather than only the sampled label when trustworthy log probabilities are available;
5. repeat ambiguous evaluations and swap candidate positions to reduce slot bias;
6. spend verification compute adaptively, concentrating it on consequential or uncertain decisions;
7. arrange stable prompt content before the changing criterion so provider prefix caching can reuse the expensive context; and
8. keep verifier output as selection evidence, not as a higher-authority instruction to the working agent.

The best initial bettercodex experiment is not a five-agent tournament or an API proxy. It is a **hidden, adaptive best-of-two selection at an existing sampling boundary**:

- sample two candidate next responses from the same immutable conversation state;
- buffer both without exposing or executing either;
- compare them against a small set of evidence-oriented criteria;
- commit only the winner to conversation history;
- execute only the winner's tool calls; and
- invoke this extra work only near finalization or at other high-risk boundaries.

This design preserves bettercodex's one-operator, one-binary, fixed-provider product direction. It also avoids the central safety problem of full trajectory best-of-N in an unsandboxed workspace: competing candidates must not execute conflicting mutations in the same checkout.

The recommendation is conditional on an empirical capability check. LLM-as-a-Verifier depends on score-token log probabilities. bettercodex's current normal Responses request does not request them, and the actual ChatGPT-authenticated route must be tested rather than assumed to support the required output shape. If usable log probabilities are unavailable, the same comparative architecture can still be evaluated with discrete pairwise selection, but one of the paper's principal advantages is then absent.

The upstream code itself should not be embedded into bettercodex. Its Python dependency stack, provider abstraction, environment-driven configuration, separate score cache, high-concurrency batch runner, and TurboAgent proxy conflict with bettercodex's deliberately focused Rust architecture. Source inspection also found correctness and packaging issues that should not be inherited.

## 1. Project identity and research question

LLM-as-a-Verifier is a general-purpose framework for converting a language model's evaluation into a fine-grained scalar reward. Its central claim is that ordinary LLM-as-a-Judge systems throw away useful information when they sample one label such as “A,” “B,” or “tie.” Instead, the framework examines the model's probability distribution over an ordered score vocabulary and computes an expected score.

The paper applies that score to three broad settings:

- **test-time selection:** generate multiple agent trajectories and choose the most promising one;
- **progress estimation:** estimate how close a running or completed trajectory is to satisfying the task; and
- **reward modeling:** use the verifier score as an outcome or process reward for optimization and reinforcement learning.

The August 2026 repository turns the first two into a reusable Python package. Its headline follow-up experiment asks a narrower and especially relevant question:

> Can the same model that generated a pool of terminal-agent trajectories distinguish its own successful trajectories from its own failures?

For Terminal-Bench 2.1, the maintainers generated five mini-swe-agent trajectories per task with `deepseek-v4-flash`, then used `deepseek-v4-flash` again as the verifier. The README reports selection above random Pass@1 for both best-of-three and best-of-five pools.

This is **self-verification across multiple rollouts**, not proof that a single rollout can reliably introspect and repair itself. The gain depends on two conditions:

1. the candidate pool must contain meaningful diversity, including at least one better trajectory; and
2. the verifier must distinguish that trajectory despite sharing some failure modes with the generator.

When all candidates fail, selection cannot create success. When all candidates succeed, verification cannot improve the task outcome. The opportunity exists on “swing” tasks where the pool contains both outcomes.

## 2. Repository scale, composition, and maturity

At the pinned snapshot, the checkout contains 936 tracked files and occupies approximately 412 MiB. Its composition is unusual:

- 903 tracked JSON files contain benchmark trajectories and labels;
- 12 Python files implement the package and scripts;
- 7 Markdown files contain the README, changelog, criteria, and adaptation instructions;
- 5 PNG files provide diagrams and plots; and
- 5 JSONL files hold MedAgentBench trajectories and progress examples.

The Python package and scripts total only a few thousand lines. The majority of repository weight is experimental data, especially MedAgentBench, SWE-Bench Verified, and Terminal-Bench 2.1 trajectories.

The project is young:

- the inspected history contains 13 commits beginning April 9, 2026;
- release `0.1.0` was uploaded to PyPI on July 7, 2026;
- release `0.2.0` was uploaded on August 14, 2026;
- the package classifier marks it Beta;
- there are no repository tags at the snapshot; and
- the visible history is essentially the work of one contributor identity, Jacky Kwok.

The repository has no checked-in test suite, no CI workflow, no dependency lockfile, and no checked-in verifier-result caches. Live model access is required to reproduce scoring. A local bytecode compilation check passed, but that is much weaker than behavioral test coverage.

### 2.1 Top-level map

| Path | Purpose |
|---|---|
| `llm_verifier/` | Reusable Python package: scoring, selection, progress, criteria parsing, benchmark registry, and loaders. |
| `scripts/` | Benchmark runner, Terminal-Bench 2.1 best-of-three and best-of-five entry points, and progress plotting. |
| `criteria/` | Three benchmark-specific verifier rubrics plus an authoring template. |
| `data/` | Previously generated trajectories and held-out binary rewards. |
| `assets/` | Project logo, method diagrams, and progress figures. |
| `README.md` | Product overview, examples, reported metrics, and conceptual explanation. |
| `CHANGELOG.md` | Two release summaries. |
| `add_new_benchmark.md` | Instructions intended to let a coding agent adapt the repository to a new trajectory dataset. |
| `pyproject.toml` | Package metadata and dependencies. |

### 2.2 What installation actually includes

The PyPI project declares Python 3.9 or newer and three unpinned runtime dependencies:

- `google-genai`;
- `openai`; and
- `tqdm`.

An optional `vllm>=0.19` extra supports self-hosting a verifier model with constrained score-token output.

A locally built wheel was approximately 40 KiB and contained the `llm_verifier` package, `py.typed`, and license metadata. It did **not** include:

- bundled `criteria/*.md` files;
- benchmark data;
- scripts; or
- repository documentation.

This creates an important difference between source-checkout use and installed-library use. Inline criteria work after installation, but a bare call such as `criteria="swe_bench"` searches for a repository-level `criteria/swe_bench.md`. From a clean working directory, the installed wheel cannot find it. The README's bundled-criteria examples are therefore not self-contained in the wheel at this snapshot.

## 3. The verification model

### 3.1 Fine-grained expected reward

The framework defines an ordered vocabulary of `G=20` score tokens. In pairwise scoring, the letters are ordered:

- `A = 20`, the best score;
- `B = 19`;
- ...
- `T = 1`, the worst score.

For task `x`, trajectory `τ`, criterion `c`, and repeated evaluation `k`, the model is prompted to emit one score token. The framework estimates:

\[
R(x,\tau)
= \frac{1}{CK}\sum_{c=1}^{C}\sum_{k=1}^{K}
\sum_{g=1}^{G} p_\theta(v_g\mid x,c,\tau)\,\phi(v_g)
\]

where `φ` maps the letter to its numeric order. The implementation then normalizes the expected value from the raw range `[1,20]` to `[0,1]`.

This differs from ordinary sampled-label judging in two ways:

1. a distribution split between adjacent scores yields an intermediate value rather than whichever label happened to be sampled; and
2. repeated evaluations and criteria can be averaged as continuous values.

The paper argues that this reduces ties, increases ranking resolution, and scales smoothly with granularity and repeated verification.

### 3.2 “Full distribution” has backend-specific limits

The conceptual formula uses the full distribution over the score vocabulary. The implementation receives at most the backend's top 20 alternatives at the relevant token position and retains alternatives that map to `A` through `T`.

It then renormalizes probability among the valid alternatives it saw. Consequences include:

- on a hosted backend, omitted score letters contribute no mass;
- non-score tokens are discarded rather than represented as uncertainty;
- upper- and lowercase forms that map to the same scalar keep the larger probability rather than summing synonymous token forms; and
- if no valid probability distribution can be found, literal output parsing is attempted, then the score falls back to `0.5`.

For self-hosted vLLM or SGLang, the implementation uses a stronger two-stage procedure: it lets the model generate analysis, then prefills each score tag and constrains the next token to one of the 20 letters. In that path, the returned distribution is much closer to a complete conditional distribution over the intended alphabet. Hosted DeepSeek and Gemini follow their own native logprob paths and do not use exactly the same constrained decoding mechanism.

The framework therefore has one mathematical interface but not one perfectly identical measurement process across providers.

### 3.3 Pairwise rather than absolute ranking

The verifier prompt presents two trajectories, `A` and `B`, and asks for one score for each under one criterion. After averaging criteria and repeats, the tournament converts the two scalar rewards into a soft preference using Bradley-Terry:

\[
p(A \succ B)=\sigma(R_A-R_B)
\]

Because both rewards are in `[0,1]`, their difference is in `[-1,1]`. The strongest possible pairwise preference is therefore only `σ(1) ≈ 0.731`, and the weakest is `σ(-1) ≈ 0.269`. This bounded strength prevents one comparison from becoming an absolute win, but it also means the returned tournament scores are **relative soft-win averages**, not calibrated probabilities that a trajectory will pass the hidden grader.

### 3.4 Why letters are used

The README illustrates integer scores for readability, but the implementation uses letters. Letter tokens make it easier to define a fixed, single-token vocabulary across tokenizers and to constrain local-model output. The pairwise and progress paths use opposite semantic directions:

- pairwise reward: `A` is best and `T` is worst;
- progress reward: `A` means 0% progress and `T` means 100% progress.

Both are internally consistent, but the inversion is a maintenance hazard because shared-looking score tokens do not carry shared meaning.

## 4. Prompt and criteria design

### 4.1 One criterion per verifier call

The project deliberately does not ask one prompt to make a single holistic judgment. Each call focuses on one criterion. The current benchmark rubrics contain three criteria each:

| Benchmark | Criteria |
|---|---|
| Terminal-Bench | Specification Adherence; Output Match; Error Signal Detection |
| SWE-Bench Verified | Root Cause Analysis; Code Quality; Empirical Verification |
| MedAgentBench | Search Parameter Accuracy; Response-Answer Alignment; FINISH Format Compliance |

The exact wording differs by benchmark, but a common principle runs through all three: **observable actions and outputs outrank the agent's own claims.** The Terminal-Bench and progress prompts explicitly warn that agents may declare victory while errors remain, change the wrong artifact, or fail to run the verification the task requires.

This is one of the project's strongest transferable concepts. Coding-agent errors often arise not because the model cannot narrate a plausible solution, but because narration is mistaken for evidence. A verifier rubric can instead ask narrow questions:

- Were all explicit requirements addressed?
- Does the observed command output support the claimed result?
- Are unresolved errors still visible?
- Did the patch target the root cause?
- Was relevant empirical validation actually run?

### 4.2 Stable prefix, variable criterion tail

Each pairwise prompt is ordered as:

1. evaluator framing;
2. optional ground-truth note;
3. task;
4. image note;
5. trajectory A;
6. trajectory B;
7. rating scale;
8. one criterion; and
9. exact output format.

The large, stable task-and-trajectories block comes before the criterion-specific text. Calls for different criteria therefore share a long prefix. The batch scorer first completes one request per distinct trajectory-pair prefix, warming the provider's prefix cache, and only then fans out remaining criteria and repeats.

The changelog reports that this raised measured prefix-cache reuse on Terminal-Bench 2.1 from 5.2% to 78.4%, reducing uncached input tokens by roughly 3.4 times. The implementation includes a thread-safe process-wide token counter for input, cached input, output, and reasoning tokens.

This is directly relevant to bettercodex. If multiple verification calls inspect the same long task history, their stable evidence should precede narrow criterion text, and usage accounting must measure rather than assume cache effectiveness.

### 4.3 Repeated evaluation and slot balancing

The batch selection path repeats each criterion `K` times. Even-numbered repetitions use the original A/B positions; odd-numbered repetitions swap the two trajectories and map the resulting scores back to candidate order. With `K>=2`, this reduces systematic preference for the first or second slot.

There are two nuances:

- the public `compare` helper repeats calls without swapping the slots, so it does not provide the same within-pair bias cancellation; and
- the random ring pass places every candidate once in A and once in B, which balances slot exposure globally even before repeated swaps.

A bettercodex adaptation should make slot swapping an explicit invariant of comparative verification rather than relying on prompt wording to eliminate positional bias.

### 4.4 Criteria authoring and IDs

Criteria files use a small Markdown format:

- optional `## Ground Truth Note`;
- `## Criteria`; and
- one `### Criterion Name` section per criterion.

IDs are generated by slugging headings or can be pinned with `{#id}`. HTML comments are stripped before model use, allowing author notes in the rubric file.

The IDs also become score-cache identifiers. That makes stable IDs useful for continuity, but the cache implementation does not hash the criterion description. Rewording a criterion while keeping its ID can silently reuse old scores. The template encourages stable IDs, so the documentation and cache semantics currently work against each other unless users manually invalidate caches.

## 5. Public API

The package exposes four principal interfaces.

### 5.1 `select`

`select(problem, candidates, ...)` chooses one candidate trajectory. Its main controls are:

- criteria, supplied inline or by criteria-file name/path;
- optional task images;
- repeated evaluations `K`, default 4;
- pivot count `k`, default 2;
- deterministic ring seed, default 0;
- model and client;
- concurrency;
- optional JSON score cache; and
- error policy, defaulting to ties.

It returns `VerifierResult` with:

- winning input index;
- winning trajectory;
- per-candidate tournament scores;
- scheduled comparison count;
- criterion IDs; and
- a deterministic best-first ranking with input-index tie-breaking.

Zero candidates raise an error. One candidate wins with score `1.0` and no comparisons.

### 5.2 `compare`

`compare` directly returns the mean fine-grained rewards `(R_A, R_B)` for one ordered pair. It raises on verifier errors rather than converting them to ties. It validates `n_evaluations>=1`, unlike `select` at this snapshot.

### 5.3 `track`

`track` scores selected checkpoints in a **completed** trajectory. A single verifier response emits all checkpoint letters, and repetitions are averaged. This makes the number of calls `O(K)` regardless of trajectory length.

The efficiency has an epistemic cost: the verifier sees the full finished trajectory while scoring earlier checkpoints. Earlier values are retrospective estimates influenced by knowledge of later events. They are useful for analysis and offline reward construction, but they are not causal online estimates.

For trajectories longer than two steps, the default checkpoint set is the interior range `2..T-1`, excluding the first and final steps unless callers specify otherwise.

### 5.4 `ProgressTracker`

`ProgressTracker` is the causal online variant. Each `update(step)` shows the verifier only the prefix available at that moment and returns a current progress estimate. Repetitions make the call count `O(TK)` for `T` updates. Because every update resends the growing prefix, total input volume can grow quadratically with trajectory length unless provider caching is highly effective.

Task images and per-step images accumulate into every later request. The repository places no explicit count or byte bound on that accumulated visual history.

## 6. Probabilistic Pivot Tournament

A full pairwise round robin requires `N(N-1)/2` undirected pairs. The project's Probabilistic Pivot Tournament, or PPT, reduces this to linear cost in `N` for a fixed number of pivots `k`.

### 6.1 Phase 1: random ring

The algorithm shuffles candidate indices into a random Hamiltonian cycle and scores every adjacent directed pair, including the final-to-first edge. There are exactly `N` ring pairs. Every candidate appears once in slot A and once in slot B.

### 6.2 Phase 2: choose pivots

Each pair contributes a Bradley-Terry soft win to both candidates. The algorithm computes each candidate's mean preference `w_i/c_i` and selects the top `k` ring leaders as pivots. Ties are broken by original candidate index.

### 6.3 Phase 3: pivot rounds

It compares:

- every non-pivot against every pivot; and
- every pair of pivots.

The scheduled comparison count is:

\[
N + k(N-k) + \binom{k}{2}
\]

The winner is the candidate with the largest mean accumulated soft win.

### 6.4 Cost and duplicate pairs

The public result's `n_comparisons` counts scheduled pair-list entries, not necessarily new model calls. A pair from the ring may also appear in the pivot round. With score caching, the duplicate is reused.

For example, best-of-three with one pivot schedules three ring pairs and two pivot-round pairs. One directed pair can overlap, so only four distinct directed pairs may require scoring. With three criteria and two repetitions, that is 24 verifier calls. The best-of-three script's docstring says “3 ring + 3 pivot-round pairs,” which does not match the current implementation.

The best-of-five script's docstring says two pivots and seven pivot-round pairs, but its current constant is one pivot. Current code therefore schedules five ring pairs plus four pivot pairs, with one possible overlap. This is documentation drift, not the algorithm described by the source.

### 6.5 When PPT is useful

PPT is valuable when candidate pools are large enough that a round robin is expensive. For `N=2`, it adds no meaningful structure. For `N=3` or `N=5`, it is still a reasonable deterministic budget allocation, but its asymptotic advantage is not the main benefit.

For bettercodex's likely first experiment—adaptive best-of-two—PPT is unnecessary. It becomes relevant only if later evidence justifies best-of-three or larger candidate pools.

## 7. Backend architecture

The package supports three verifier routes behind one scoring interface.

### 7.1 Backend selection

If no explicit client is supplied, environment discovery uses this order:

1. `OPENAI_BASE_URL` for an OpenAI-compatible server;
2. `DEEPSEEK_API_KEY` for hosted DeepSeek; then
3. `VERTEX_API_KEY` for Gemini through Vertex AI.

The constant default model is `gemini-2.5-flash`, but a tagged DeepSeek client replaces that with `deepseek-v4-flash`, and a local OpenAI-compatible client can query its served model ID. These environment rules mean the effective verifier can differ from the model argument a caller may think is in use.

The simple `.env` reader accepts unquoted `KEY=value` lines and does not implement full dotenv syntax.

### 7.2 Gemini through Vertex AI

The repository uses `google-genai` in Vertex AI mode because its documentation states that the plain Gemini API does not expose the token-level log probabilities required by this method. Thinking is disabled for this path. Images are attached as task context.

### 7.3 Hosted DeepSeek

Hosted DeepSeek uses an OpenAI-compatible client pointed at `https://api.deepseek.com`, model alias `deepseek-v4-flash`, high reasoning effort by default, and a 32,768-token completion budget. Environment variables can change effort and output length.

The output budget includes reasoning and final score tags. If reasoning consumes the budget before the tags are produced, the implementation raises instead of silently accepting missing log probabilities. Default DeepSeek concurrency is 500, while the Terminal-Bench 2.1 benchmark configuration sets 2,000 workers.

### 7.4 Local OpenAI-compatible servers

For vLLM or SGLang, the package first requests analysis and then performs one constrained, single-token continuation per score tag. It uses a `structured_outputs.choice` constraint over the 20 letters. This requires the optional vLLM version noted by the package metadata.

The route is called “OpenAI-compatible,” but practical compatibility requires more than accepting Chat Completions: the server must return token-level log probabilities and, for the constrained path, support the prefill/structured-output behavior used by the implementation.

### 7.5 Backend semantics are not identical

The same public API can therefore mean:

- no thinking on Gemini;
- high-effort reasoning on DeepSeek; or
- free-form analysis followed by constrained score-token calls on a local server.

The returned scalar is normalized into one range, but latency, cost, reasoning depth, score-token conditioning, and probability completeness differ. Cross-backend results should not be treated as measurements from one interchangeable verifier instrument without calibration.

## 8. Caching, concurrency, and failure behavior

### 8.1 Two different caches

The project relies on two distinct mechanisms:

1. **provider prefix caching**, encouraged by stable prompt layout and warm-up ordering; and
2. **local JSON score caching**, which stores completed criterion/repetition scores for directed trajectory pairs.

They solve different problems. Prefix caching reduces the cost of live requests. The JSON cache avoids sending a request at all.

### 8.2 Score-cache key weakness

A cache key contains only:

- criterion ID;
- task name;
- candidate indices `a,b`; and
- repetition number.

It does not contain:

- task text;
- candidate content or hashes;
- criterion description;
- prompt-template version;
- score granularity;
- model;
- backend; or
- reasoning settings.

Reusing a cache path after changing data, rubric wording, prompt code, or model can silently produce stale mixed results. A production-quality port should use a content-addressed key that commits to every input affecting the score.

### 8.3 Cache persistence

The batch scorer periodically rewrites the full JSON object directly to the destination path and writes it again at completion. It does not use a temporary file plus atomic replacement. Process interruption or storage failure can therefore leave a truncated cache.

Failed calls are not persisted, which is good: transient ties do not become permanent evidence.

### 8.4 Default fail-open policy

`select` defaults to `on_error="tie"`. A failed verifier call becomes `0.5/0.5` for the current run, and only the first few errors are printed when progress output is enabled. This keeps long experiments running, but widespread backend failure can produce a valid-looking ranking dominated by fallback ties and deterministic input-index tie-breaking.

For an interactive coding agent, verifier infrastructure failure should not silently masquerade as a quality judgment. A bettercodex experiment should expose a clear degraded mode and either use the unverified primary sample or ask for operator confirmation, depending on the boundary.

### 8.5 Public `select` phase-merging defect

Direct source inspection found a correctness issue in the default no-cache path of `select`:

1. phase A calls `score_pairs(ring)` and receives ring scores;
2. ring scores are used to choose pivots;
3. phase B calls `score_pairs(pr_pairs)` and receives a new score dictionary;
4. the local `score` closure is rebound to phase B's dictionary; and
5. final aggregation evaluates both ring and pivot pairs through that phase-B-only dictionary.

When `cache=None`, phase B starts from an empty cache, so most phase-A ring entries are absent and default to ties during final aggregation. Ring evidence still affects pivot choice, but it is mostly discarded from the final winner score. If a cache file is supplied, phase B reloads phase A from disk and the behavior changes.

The benchmark runner always supplies a cache path, so its normal two-phase benchmark flow preserves the ring scores. The public library's default `select` path does not. This finding is specific to the inspected snapshot and should be rechecked before relying on a later release.

### 8.6 Input validation gaps

At this snapshot:

- `select` does not reject `n_evaluations=0`; it degenerates toward tie defaults;
- negative pivot counts are not explicitly rejected; and
- `compare`, `track`, and `ProgressTracker` do validate positive repetitions.

These are small library-hardening gaps, but they reinforce that the package is research-stage rather than a production control plane.

## 9. Image support and operational safety

Every public scoring interface accepts:

- local image paths;
- HTTP or HTTPS URLs;
- raw bytes; or
- sequences of those forms.

The loader recognizes PNG, JPEG, GIF, and WebP magic bytes, defaulting unknown data to PNG. Images are base64-encoded or attached in the provider's native format.

The repository and README call this multimodal support. More precisely, the implementation supports text plus image collections. Paper experiments may represent video as sampled frames, but there is no audio path and no general video decoding pipeline in this repository.

Remote image loading uses a direct URL open and reads the response into memory without an explicit timeout, size bound, redirect policy, or content-type validation. In a trusted research script this may be acceptable. It should not be copied into a privileged, unsandboxed agent harness.

## 10. Progress estimation

### 10.1 The progress question

The progress prompt asks one narrow question at each checkpoint:

> Given everything the agent has done up to this point, would the current state satisfy the hidden grader?

It explicitly rejects effort, confident narration, and step count as evidence. It permits progress to plateau or decrease when the agent pursues a wrong path or regresses.

This framing is valuable beyond its exact scalar implementation. Many harnesses infer progress from activity: more tool calls, more files edited, or a confident status update. LLM-as-a-Verifier instead asks whether observed state increasingly supports task completion.

### 10.2 Offline versus online semantics

The project correctly distinguishes:

- `track`: retrospective, full-trajectory scoring of multiple checkpoints in a small number of calls; and
- `ProgressTracker`: causal, prefix-only scoring while an agent runs.

These should not be mixed in evaluation. An offline curve can benefit from hindsight and is unsuitable for validating an online early-stop policy without further controls.

### 10.3 Missing-score behavior and plotting

Individual unparseable checkpoint values are omitted when repetitions are averaged. If all repetitions are missing, the aggregate becomes `0.5`.

The demonstration plotting script maps missing raw points to zero before plotting and then normalizes all curves by the global maximum of their means. That visualization transformation is not the same as the library's `0.5` all-missing fallback and should not be interpreted as raw calibrated progress.

The plotting script imports NumPy and Matplotlib, but neither is declared as a package dependency or optional extra.

## 11. Shipped criteria and data

The repository's largest contribution to reproducibility is not code but a fixed collection of candidate trajectories with binary success labels.

### 11.1 Dataset inventory

| Dataset | Approximate size | Tasks | Candidate trials | Pool | All-pass | Swing | All-fail |
|---|---:|---:|---:|---:|---:|---:|---:|
| Terminal-Bench 2.0, Capy/GPT-5.5 | 8.1 MB | 89 | 445 | 5 | 62 | 20 | 7 |
| Terminal-Bench 2.1, mini-swe-agent/DeepSeek | 57.7 MB | 89 | 445 | 5 | 50 | 36 | 3 |
| Terminal-Bench 2.1, first three only | same source | 89 | 267 | 3 | 58 | 24 | 7 |
| SWE-Bench Verified | 193.6 MB | 500 | 1,500 | 3 | 336 | 86 | 78 |
| MedAgentBench | 103.6 MB | 300 | 1,500 | 5 | 187 | 38 | 75 |
| Progress examples | 71.8 kB | 2 examples | — | — | — | — | — |

“Swing” means the candidate pool contains both successes and failures. Only those tasks can reveal a selection improvement over arbitrary candidate choice.

The audited binary reward totals are:

- Terminal-Bench 2.0: 370 pass, 75 fail;
- Terminal-Bench 2.1: 350 pass, 95 fail;
- SWE-Bench Verified: 1,141 pass, 359 fail; and
- MedAgentBench: 1,053 pass, 447 fail.

### 11.2 Trajectory lengths

The verifier often sees very large strings:

| Dataset | Mean formatted trace | Median | 95th percentile | Maximum |
|---|---:|---:|---:|---:|
| Terminal-Bench 2.0 | 9,495 chars | 7,564 | 24,732 | 43,145 |
| Terminal-Bench 2.1 | 107,611 chars | 88,335 | 311,171 | 552,945 |
| SWE-Bench Verified | 31,724 chars | — | — | 116,246 |
| MedAgentBench | 3,942 chars | — | — | 25,692 |

This explains why prompt layout and prefix caching are central rather than incidental optimizations. One Terminal-Bench 2.1 comparison can carry two trajectories totaling hundreds of thousands of characters before repeated criteria and reasoning output are considered.

### 11.3 Terminal-Bench loader

The Terminal-Bench loader:

- discovers task subdirectories and `*_trajectory.json` files;
- holds binary reward labels out of the verifier prompt;
- extracts the first captured user task message;
- keeps agent messages, command keystrokes, and observed outputs;
- drops system and user messages from the formatted trajectory; and
- does not truncate the resulting trace.

The raw data also includes trial IDs, checksums, timing metadata, and terminal trajectory schemas that the verifier does not receive.

At the inspected snapshot, the benchmark registry's Terminal-Bench 2.0 path is stale. It points to `data/terminal_bench_trajs/capy_gpt-5.5`, while the directory on disk is `data/terminal_bench_2.0_trajs/capy_gpt-5.5`. The registered command therefore loads zero tasks and would later divide by zero in reporting unless the path is corrected.

### 11.4 SWE-Bench loader

The SWE loader discovers run directories, retains instances present in at least two runs, extracts the PR description and instructions, and constructs one candidate per run. It:

- truncates each assistant message to 2,000 characters;
- truncates each tool or follow-up user output to 2,000 characters;
- removes duplicated PR/instruction blocks from trajectory content; and
- appends the final code patch without a comparable total-size bound.

The reward remains held out for final metric computation.

### 11.5 MedAgentBench loader

The Med loader combines task instructions and context from `problems.json`, then reads five run directories of JSONL trajectories and corresponding result files. Agent messages are preserved. FHIR responses and feedback are truncated to 3,000 characters each. A status string determines the binary reward.

### 11.6 Data quality and provenance boundaries

Deterministic inspection found:

- stable problem text across trials within each current task;
- no empty or fallback problem strings in the current loaded datasets;
- no duplicate trial names in the current data; and
- the expected fixed pool sizes.

However, the repository does not include:

- the candidate-generation agents and environments;
- immutable model snapshot identifiers for every generator;
- a data manifest with checksums and provenance narrative;
- terminal container images or objective graders;
- generation commands for all trajectory pools; or
- verifier caches and result artifacts from the reported runs.

The data permits rescoring fixed trajectories. It does not independently reproduce their generation or every underlying benchmark label.

## 12. Benchmark runner and metric semantics

### 12.1 Classification by held-out reward

The benchmark runner classifies each task using ground-truth candidate rewards:

- **all-pass:** every candidate succeeds;
- **swing:** candidates disagree; or
- **all-fail:** every candidate fails.

It runs the verifier tournament only on swing tasks. All-pass tasks are counted as successful without verification, and all-fail tasks as failures without verification.

This is mathematically valid for measuring selection accuracy: no selector can change the outcome on homogeneous pools. It is not a deployment-cost estimate. A live system does not know whether a new candidate pool is all-pass or all-fail without an objective grader. Always-on production verification would spend compute on those tasks too, or would need a separate gate.

### 12.2 Reported metrics

The runner computes:

- **Pass@1:** mean success probability from taking one pool member without verifier selection;
- **LLM-as-a-Verifier:** success count after selecting on swing tasks plus all-pass tasks; and
- **Oracle:** all tasks whose pool contains at least one success.

The Oracle is a pool-quality ceiling, not a realizable verifier. The gap from Pass@1 to Oracle measures how much candidate diversity is available; the verifier can only recover part of that gap.

### 12.3 Current defaults versus paper settings

The paper's main benchmark experiments commonly use `K=8` repeated evaluations. Package and benchmark defaults were later reduced to `K=4`, while Terminal-Bench 2.1 uses `K=2`. README reproduction commands for the older benchmarks do not automatically restore the paper's `K=8`; callers must pass it explicitly.

The README says Gemini 2.5 Flash is the verifier for the older benchmark table, but `scripts/run.py` has no model flag and the benchmark registry has no model field. Backend selection follows environment precedence. A `.env` containing a DeepSeek key and no explicit OpenAI base URL will therefore use DeepSeek instead of Gemini. Reproducing the stated backend requires controlling the environment, not only running the documented command.

### 12.4 Self-verification scripts and uncertainty

The README reports:

| Configuration | Pass@1 | Verifier | Oracle |
|---|---:|---:|---:|
| Best-of-3 | 79.4% | 86.5% ± 1.1% | 92.1% |
| Best-of-5 | 78.7% | 88.0% ± 0.6% | 96.6% |

The checked-in best-of-three and best-of-five scripts each run one seed at a time and do not aggregate multiple seeds or compute uncertainty. The repository contains no result artifacts explaining the `±` values. Those values may come from external repeated runs, but the checked-in scripts do not by themselves reproduce the aggregate statistic.

## 13. Paper evidence

The paper evaluates more than the repository implements. Its results are useful evidence for the principles, but they should be attributed to the paper rather than presented as locally reproduced facts.

### 13.1 Controlled verification scaling

On a controlled set of 200 Terminal-Bench trajectories, the paper reports:

- increasing score granularity from `G=1` to `G=20` improves verification accuracy from 73.1% to 77.5%;
- increasing repeated evaluations from `K=1` toward `K=16` improves performance to roughly 77.5%;
- individual criteria score approximately 75.2% to 76.4%, while their ensemble reaches 78.3%; and
- a discrete judge has a 26.7% tie rate at `K=1`, while continuous reward has no exact ties in that comparison.

A query-optimization experiment repeated the same evaluation 100 times. At granularity five, the discrete judge reportedly selected correctly 12 times and tied 88 times, whereas continuous scoring selected correctly 69 times with no ties. At granularity 20, continuous scoring selected correctly 77 times.

These results support the claim that distributional scoring provides more ranking resolution than one sampled class. They do not establish that every backend's top-logprob approximation is equally calibrated.

### 13.2 Fixed-pool test-time selection

The paper reports these main test-time selection results:

| Benchmark | Pass@1 | Verifier | Oracle |
|---|---:|---:|---:|
| Terminal-Bench 2.0, best-of-5 | 83.1% | 86.5% | 92.1% |
| SWE-Bench Verified, best-of-3 | 76.1% | 78.2% | 84.4% |
| MedAgentBench, best-of-5 | 70.2% | 73.3% | 75.0% |

The gain is largest where candidate diversity and verifier discrimination align. None reaches Oracle, demonstrating remaining correlated verifier errors.

The paper also reports generalization across different terminal harnesses, with stronger selection than baseline judging on Terminus-Kira and Terminus-2 pools. This is important because a verifier that only recognizes one generator's style would be much less useful.

### 13.3 Tournament-cost study

For pools of 20 candidates over 89 tasks, the paper reports a gradual trade-off as pivots increase:

| Pivots | Pair evaluations | Reported score |
|---:|---:|---:|
| 1 | 2,570 | 65.83 |
| 3 | 4,723 | 66.17 |
| 5 | 6,609 | 66.27 |
| 7 | 8,242 | 66.67 |
| 9 | 9,630 | 67.13 |
| Full round robin | 13,111 | 67.42 |

This supports PPT as a compute-quality trade-off rather than a lossless replacement for round robin.

### 13.4 Progress and reward-model experiments

The paper reports progress-estimation agreement using Value-Order Correlation (VOC), the Spearman rank correlation between checkpoint order and predicted prefix value, on successful and failed Terminal-Bench trajectories and on RoboRewardBench. It also evaluates process-reward selection in TauBench and Terminal-Bench, outcome-reward selection on SWE-Bench Lite, AIME, and HMMT, and reinforcement learning on LIBERO and MATH.

Reported examples include:

- Terminal progress value-of-critique around `0.848±0.012` for successful and `0.769±0.016` for failed trajectories;
- RoboReward VOC `0.966`;
- RoboRewardBench pair-preference accuracy of 87.4% with mean absolute error 0.72;
- process-reward action selection improving with more verifier samples on TauBench and Terminal-Bench;
- outcome-reward selection improving SWE-Bench Lite from 23.5 to 33.0, AIME from 71.5 to 90.0, and HMMT from 52.0 to 73.3;
- LIBERO reinforcement learning reaching about 1.8 times the sample efficiency of the comparison reward and a higher final success rate; and
- a smaller MATH GRPO sample-efficiency improvement.

The public repository does not contain the robotics, TauBench, AIME, HMMT, SAC, or GRPO implementations required to reproduce those claims.

### 13.5 Paper limitations

The paper explicitly acknowledges several boundaries:

- the method requires access to scoring logits or log probabilities;
- evaluation criteria are hand-designed;
- repeated evaluations use fixed rather than adaptive budgets; and
- reinforcement-learning experiments are limited relative to long-horizon, multi-turn deployment.

Those limitations are central to a bettercodex decision. The verifier is not a free confidence oracle; it is an additional model computation whose behavior depends on rubric quality, endpoint capabilities, candidate diversity, and budget allocation.

## 14. Paper-versus-repository coverage

The following table prevents the paper's scope from being conflated with the checked-in implementation.

| Capability or experiment | In this repository? | Notes |
|---|---|---|
| Text pairwise fine-grained reward | Yes | Core package implementation. |
| Image-conditioned scoring | Yes | Image paths, URLs, or bytes. |
| Best-of-N selection | Yes | PPT implementation and public `select`. |
| Offline progress curves | Yes | `track`. |
| Online causal progress | Yes | `ProgressTracker`. |
| Terminal-Bench 2.0 trajectories | Yes | Registry path is stale at the snapshot. |
| Terminal-Bench 2.1 self-verification trajectories | Yes | Five trajectories per task. |
| SWE-Bench Verified trajectories | Yes | Three runs per retained task. |
| MedAgentBench trajectories | Yes | Five runs per task. |
| Candidate-generation agents | No | Fixed trajectories only. |
| Terminal containers and graders | No | Rewards are already present. |
| RoboRewardBench loader/data | No | Paper-only or external. |
| Native video pipeline | No | Image lists can represent sampled frames. |
| TauBench/AIME/HMMT evaluation code | No | Paper-only or external. |
| SAC/GRPO training code | No | Paper-only or external. |
| Full paper experiment harness | No | Only selected benchmark scripts ship. |
| TurboAgent API proxy | No | Separate companion repository. |
| Claude Code or Codex integration | No | TurboAgent is external. |

## 15. TurboAgent: related but separate

The README links [TurboAgent][turbo-agent], a separate project that places an API proxy between an Anthropic- or OpenAI-compatible client and its model provider. It generates multiple candidate responses, verifies them, and replays the selected response as streaming events. It also includes a visualizer.

TurboAgent demonstrates one deployment pattern for LLM-as-a-Verifier, but it is not part of the inspected repository and should not be mistaken for the framework's core package. Its proxy/server/configuration architecture is also a poor direct fit for bettercodex, whose product direction deliberately excludes an app server, provider framework, plugin system, and general configuration layer.

The useful concept is the interception point—buffer alternatives before committing one—not the proxy implementation.

## 16. Reproducibility and engineering assessment

### 16.1 Strengths

The repository has several strong research-engineering qualities:

- the core method is compact and readable;
- mathematical concepts map clearly to code;
- fixed trajectory pools are shipped rather than described abstractly;
- rewards are held out of verifier prompts;
- prompt-prefix optimization is implemented and token usage is measured;
- ring sampling is deterministic under a seed;
- failed scores are not persisted;
- criteria are editable data rather than hard-coded benchmark branches;
- public APIs support caller-supplied clients; and
- the README explains both high-level use and the tournament mechanism.

### 16.2 Weaknesses and drift

The main limitations at the snapshot are:

1. no automated tests or CI;
2. no dependency lockfile or upper bounds;
3. source-checkout examples that do not work from the built wheel because criteria are omitted;
4. a stale Terminal-Bench 2.0 registry path;
5. no-cache behavior in public `select` that discards most ring evidence during final aggregation;
6. cache keys that do not commit to model, content, prompt, or criterion wording;
7. non-atomic cache writes;
8. fail-open ties that can conceal widespread verifier failure;
9. best-of-three and best-of-five script documentation that disagrees with current constants;
10. uncertainty values not reproduced by the checked-in single-seed scripts;
11. environment-driven backend selection that can silently change the verifier model; and
12. missing code and artifacts for a substantial portion of the paper.

These findings do not invalidate the research idea or prove the reported metrics false. They mean the repository should be read as a fast-moving research artifact whose concepts require independent implementation and evaluation before becoming a production harness invariant.

## 17. What the project teaches about verification

Several broader conclusions emerge from the code and evidence.

### 17.1 Verification is a selection mechanism, not truth

The verifier produces a relative preference conditioned on a prompt and its visible evidence. It does not inspect hidden filesystem state unless that state is represented in the trajectory, and it can share blind spots with the generator. Self-verification gains show useful discrimination, not independence or certainty.

### 17.2 Candidate diversity is a prerequisite

Best-of-N quality is bounded by Oracle. A stronger selector cannot recover a correct trajectory that was never generated. Verification and generation diversity must be designed together.

### 17.3 Objective evidence should precede model judgment

Tests, compiler output, lint results, file diffs, and command exit status are usually more authoritative than an LLM's estimate. The project's prompts recognize this by prioritizing observed output. A production harness should go further: run deterministic checks where available, then ask the verifier to reason over the remaining ambiguity.

### 17.4 Comparative judgments are easier to calibrate

A model may be poor at assigning “82% likely correct” in isolation but better at explaining why one of two concrete attempts better satisfies a requirement. Pairwise scoring turns that comparative ability into a ranker.

### 17.5 Decomposition trades tokens for diagnosis

Separate criteria reduce the risk that one salient dimension dominates a holistic score. They also multiply calls. This is worthwhile only when criteria correspond to distinct failure modes and the backend can reuse the shared prefix.

### 17.6 Repetition should be adaptive

The paper scales fixed `K`; its own limitations identify adaptive evaluation as future work. In a harness, repeated scoring should concentrate on close comparisons, parse failures, criterion disagreement, or high-risk actions rather than applying the maximum budget to every response.

### 17.7 Cache design is part of correctness

Prefix arrangement affects cost. Local score-key design affects correctness. Both need first-class treatment. A fast cache that can return scores for the wrong prompt or candidate is worse than no cache.

## 18. Mapping the ideas onto bettercodex

### 18.1 Current bettercodex constraints

bettercodex is a focused Rust port of Codex, not a general verifier platform. Its current product boundaries include:

- one `bcodex` binary and one operator;
- ChatGPT-authenticated normal Responses;
- exactly three GPT-5.6 model choices;
- exactly four ordinary function tools plus hosted web search;
- no provider framework, app server, SDK, plugin system, MCP layer, or configuration framework;
- incremental stable history for prompt caching;
- bounded model-visible items;
- transactional compaction; and
- unsandboxed command and file execution with the invoking user's permissions.

Those constraints rule out a direct port of the Python package or TurboAgent. They do not rule out an internal verification phase inside the existing turn loop.

### 18.2 Current sampling behavior

The current turn loop requests one model response, records usage, and either returns its final answer or executes its tool calls. During sampling, completed Responses items are streamed through a channel and appended to conversation history as they arrive. After the response, the request history is restored around the newly installed items.

This is correct for one visible sample. It is incompatible with hidden competing candidates because a losing candidate must not:

- enter model history;
- appear in the visible transcript as the chosen agent action;
- alter the WebSocket or previous-response baseline used by later requests;
- emit tool lifecycle entries;
- execute commands or edits; or
- become part of saved-session resume state.

A verifier experiment therefore requires a genuinely isolated response collector, not a loop that calls the existing sampling path twice.

### 18.3 Responses request capability

bettercodex's current request includes encrypted reasoning content but no output-text logprob field. Before designing expected-logprob scoring, a task-owned capability probe should determine whether the ChatGPT-authenticated normal Responses endpoint returns usable per-token alternatives for the selected GPT-5.6 models.

The probe must answer:

1. can the route request top log probabilities at all?
2. are alternatives available for a constrained score position?
3. can a score alphabet be made reliably single-token?
4. does streaming preserve the relevant fields?
5. how much latency and token cost does reasoning add?
6. do Sol, Terra, and Luna differ materially as verifiers?

If the answer is no, bettercodex can still test pairwise discrete selection, but it should not claim to have ported the fine-grained reward method.

## 19. Recommended bettercodex experiment

### 19.1 Evaluation target

The first question should be narrow:

> Does hidden pairwise selection improve observable coding-task outcomes enough to justify its added latency and tokens?

Do not begin by adding a user-facing mode, configuration surface, or permanent model-facing rubric. Run an offline or task-owned evaluation first.

### 19.2 Candidate boundary

The safest useful boundary is immediately before committing a model response:

1. freeze the same admitted conversation history and world state;
2. sample candidate A without installing its output;
3. sample candidate B from the identical baseline;
4. reject malformed or policy-incompatible candidates deterministically;
5. compare remaining candidates using task, current transcript evidence, proposed tool actions, and a small rubric;
6. commit one winner; and
7. execute only that winner's tool calls.

This is best-of-two **next-action or final-response selection**, not full parallel trajectory execution. It avoids competing filesystem mutations.

### 19.3 Adaptive trigger

Always doubling every sample would be expensive and would degrade responsiveness. Verification should initially trigger only when one of these conditions holds:

- the model is about to end the turn after making changes;
- the answer claims tests or requirements are satisfied;
- visible tool output contains errors, warnings, failing tests, or ambiguity;
- the action is consequential and hard to reverse;
- no objective validation was run despite code changes; or
- a cheap first-pass verifier reports low margin or criterion disagreement.

The trigger itself must be evaluated. A progress score should not become an opaque autonomous stop signal before its false-positive and false-negative costs are measured.

### 19.4 Initial criteria

A compact coding rubric could use three independent criteria:

1. **Requirement coverage:** Does the proposed response address every explicit user and repository requirement relevant to this step?
2. **Evidence validity:** Are claims supported by actual tool output, diffs, tests, or inspected source rather than narration?
3. **Unresolved risk:** Does the response ignore errors, uncertainty, incomplete validation, unsafe side effects, or an incorrect root cause?

A fourth “scope and maintainability” criterion may be useful later, but starting with too many criteria multiplies cost and can dilute the primary correctness signal.

### 19.5 Scoring policy

For a first online design:

- use one pairwise comparison when the margin is clear;
- perform a swapped-position repetition when the first comparison is close or consequential;
- aggregate criteria separately and expose disagreement to internal telemetry;
- prefer deterministic objective checks over verifier opinion;
- cap total verification rounds per turn; and
- on verifier failure, fall back explicitly to the primary sample rather than treating infrastructure failure as a tie-derived winner.

If top log probabilities are available, compute expected values over a fixed score alphabet. The implementation should sum probability mass for equivalent token forms and record how much total probability was represented, rather than silently renormalizing an arbitrary subset without diagnostics.

### 19.6 History, transport, and usage requirements

Any prototype must preserve bettercodex's inference invariants:

- candidate requests begin from the exact same immutable history cursor;
- losing output never enters conversation history;
- only winning reasoning continuity is retained;
- all candidate and verifier usage is counted;
- cache and context-window lineage advance only for installed output;
- cancellation abandons all hidden responses safely;
- retries cannot install a partial losing stream;
- saved sessions resume with only the selected branch;
- tool-call IDs and outputs belong only to the winner; and
- terminal streaming makes it clear when the user is waiting for selection.

The current sampling implementation incrementally appends completed items, so this is a meaningful inference refactor. A candidate-isolated collector must be designed alongside the Responses transport and upstream Codex behavior rather than bolted onto `run_turn`.

### 19.7 Trust boundary

Verifier output is another model output. It must not be promoted to developer authority or injected as trusted harness policy. The safest initial use is a small internal selection record:

- candidate A score;
- candidate B score;
- criterion margins;
- selected index; and
- verifier failure/degraded status.

If later designs feed critique back to the agent, that critique must remain clearly attributed lower-authority data and must be protected from trajectory-contained prompt injection. The verifier's access to repository files and tool output does not grant those contents instruction authority.

## 20. Phased adoption plan

### Phase 0: capability and cost probe

Use task-owned temporary evaluation code to test logprob availability and response shapes on the actual ChatGPT-authenticated route. Measure latency, input tokens, cached input, output tokens, and failure modes for each available bettercodex model.

**Exit condition:** a documented, reproducible signal format suitable for scoring, or a decision to evaluate discrete pairwise selection instead.

### Phase 1: offline shadow evaluation

Collect or construct fixed bettercodex-style candidate responses without changing production behavior. Score them against objective task outcomes and compare:

- primary sample;
- discrete pairwise judge;
- expected-logprob pairwise verifier, if available;
- one versus multiple criteria;
- one versus swapped repetition; and
- Sol, Terra, and Luna as verifier choices.

Measure selection accuracy only on swing pairs, but separately estimate always-on deployment cost.

**Exit condition:** statistically credible improvement over primary-sample selection on representative coding tasks.

### Phase 2: hidden finalization best-of-two

Restrict online selection to responses that would otherwise end the turn. This avoids competing tool execution and tests whether the method improves final correctness, completeness, and reporting quality with the smallest runtime change.

**Exit condition:** measurable quality gain without unacceptable latency, context, or resume regressions.

### Phase 3: adaptive next-action selection

Permit hidden candidate tool-call responses from one immutable state and execute only the winner. Add strict tests around history isolation, tool lifecycle, cancellation, retries, and transport baselines.

**Exit condition:** improved task success, not merely better prose, under realistic tool-use workloads.

### Phase 4: larger pools only if justified

Evaluate best-of-three and PPT only if best-of-two leaves substantial Oracle headroom and candidate diversity is demonstrably useful. Do not assume more candidates are automatically better after accounting for generation cost and correlated failures.

### Phase 5: progress signals as advisory telemetry

Only after selection is validated should online progress estimates be considered for early stopping, resampling, or escalation. Initially expose them as telemetry, not an autonomous control policy.

## 21. Concepts that should not be ported

### 21.1 The Python package and provider layer

Embedding Python, `google-genai`, `openai`, vLLM integration, `.env` backend discovery, or a new package runtime would expand bettercodex's dependency and product surface without necessity. The algorithm is small enough to express natively if evaluation supports it.

### 21.2 TurboAgent's proxy/server architecture

bettercodex does not need another API server, compatibility proxy, visualizer service, or configuration framework. The relevant interception point already exists inside its inference loop.

### 21.3 Full parallel mutable trajectories

Running two or five independent coding agents in the same unsandboxed working directory would create races, conflicting edits, duplicate commands, and ambiguous rollback. Full trajectory best-of-N would require isolated worktrees or sandboxes plus a deliberate merge policy, which is far beyond the smallest useful port.

### 21.4 Always-on maximum verification

Fixed best-of-five with multiple criteria and repetitions can multiply both input and output cost. bettercodex should not sacrifice its interactive responsiveness before proving where verification has positive expected value.

### 21.5 Verifier criticism as elevated instruction

A model-generated critique should never become developer-authority context merely because it is called a verifier. Selection evidence and instruction authority are separate concerns.

### 21.6 Audio or video expansion

The repository's image support and paper's robotics framing do not justify adding audio or video modalities to bettercodex. They are outside current product scope and unnecessary for the coding-quality opportunity.

## 22. Decision matrix

| Idea | Expected value for bettercodex | Fit | Recommendation |
|---|---|---|---|
| Evidence over narration | High | Excellent | Adopt as evaluation principle. |
| Narrow criteria decomposition | High | Good, but costs calls | Evaluate with 2–3 criteria. |
| Pairwise candidate comparison | High | Good | Primary experiment. |
| Expected score-token logprobs | Potentially high | Endpoint-dependent | Capability probe first. |
| A/B slot swapping | Medium to high | Easy once pairwise exists | Make an invariant. |
| Adaptive repeated verification | High | Excellent | Prefer over fixed maximum `K`. |
| Stable-prefix prompt layout | High cost benefit | Excellent | Preserve in any verifier prompt. |
| PPT | Medium for larger pools | Premature | Defer until `N>=3` is justified. |
| Offline progress tracking | Medium for analysis | Good | Possible evaluation tool. |
| Online progress control | Uncertain | Risky without calibration | Telemetry only at first. |
| Python/provider framework | Low | Poor | Do not port. |
| TurboAgent proxy | Low | Poor | Do not port. |
| Full mutable trajectory BoN | Potential quality, very high complexity | Poor initially | Reject as first step. |
| Verifier as trusted instruction source | Negative | Violates trust model | Do not port. |

## 23. Final assessment

LLM-as-a-Verifier's core insight is credible and relevant: an agent can often discriminate between concrete candidate outcomes more reliably than it can produce a perfectly calibrated one-shot self-assessment, especially when the verifier is forced to attend to observable evidence, score narrow criteria, and expose more of its score distribution than one sampled label.

The self-verification result is particularly suggestive for bettercodex because it shows that verifier and generator do not have to be different model families for selection to help. It does **not** show that same-model verification is independent, universally reliable, or worth its cost in every turn.

For bettercodex, the practical opportunity is a small internal decision layer, not a new platform:

- generate limited alternatives from one immutable state;
- compare them with evidence-oriented criteria;
- exploit logprob expectations if the real endpoint supports them;
- repeat only when uncertainty warrants it;
- commit one branch before side effects; and
- measure actual task success, latency, token cost, cache behavior, and failure modes.

The upstream repository should be treated as a research prototype and dataset bundle. It provides a useful algorithmic vocabulary and promising evidence, while its packaging gaps, cache semantics, phase-merging defect, documentation drift, absent tests, and incomplete paper reproduction make direct code reuse inappropriate.

The concise recommendation is therefore:

> **Port the verifier principles into an isolated bettercodex evaluation, beginning with adaptive hidden best-of-two finalization or next-action selection. Do not port the Python framework, proxy architecture, provider abstraction, or full multi-trajectory execution system. Ship nothing until the actual Responses route, quality gain, and cost profile are measured.**

## Source index

### Primary project sources

- [LLM-as-a-Verifier repository][repo]
- [Pinned source snapshot][snapshot]
- [README at the pinned snapshot][readme]
- [Package metadata][pyproject]
- [Core fine-grained reward implementation][fine-grained]
- [Public API][public-api]
- [Probabilistic Pivot Tournament][ppt]
- [Progress tracking][progress]
- [Benchmark registry][benchmarks]
- [Dataset loaders][loaders]
- [Benchmark runner][runner]
- [Criteria parser][prompts]
- [Terminal-Bench criteria][terminal-criteria]
- [SWE-Bench criteria][swe-criteria]
- [MedAgentBench criteria][med-criteria]
- [Project changelog][changelog]

### Research and companion sources

- [LLM-as-a-Verifier paper][paper]
- [PyPI package][pypi]
- [TurboAgent companion repository][turbo-agent]
- [Project documentation site][docs-site]

### bettercodex context used for the transfer analysis

- [`docs/product-direction.md`](../docs/product-direction.md)
- [`docs/inference.md`](../docs/inference.md)
- [`docs/instruction-hierarchy.md`](../docs/instruction-hierarchy.md)
- [`src/agent.rs`](../src/agent.rs)
- [`src/api.rs`](../src/api.rs)

[repo]: https://github.com/llm-as-a-verifier/llm-as-a-verifier
[snapshot]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/tree/115de305f23ed89bc42e86e010853c40059f3f7d
[readme]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/README.md
[pyproject]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/pyproject.toml
[fine-grained]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/llm_verifier/fine_grained_reward.py
[public-api]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/llm_verifier/__init__.py
[ppt]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/llm_verifier/pivot_tournament.py
[progress]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/llm_verifier/progress.py
[benchmarks]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/llm_verifier/benchmarks.py
[loaders]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/llm_verifier/loaders.py
[runner]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/scripts/run.py
[prompts]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/llm_verifier/prompts.py
[terminal-criteria]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/criteria/terminal_bench.md
[swe-criteria]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/criteria/swe_bench.md
[med-criteria]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/criteria/medagentbench.md
[changelog]: https://github.com/llm-as-a-verifier/llm-as-a-verifier/blob/115de305f23ed89bc42e86e010853c40059f3f7d/CHANGELOG.md
[paper]: https://arxiv.org/abs/2607.05391v2
[pypi]: https://pypi.org/project/llm-verifier/0.2.0/
[turbo-agent]: https://github.com/llm-as-a-verifier/TurboAgent
[docs-site]: https://llm-as-a-verifier.com/docs/
