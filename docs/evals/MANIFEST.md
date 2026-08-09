# Simple Task-Specific Evals Docs Manifest

Use this file as a routing map for constructing small, task-specific evaluators. Prefer the live OpenAI docs linked here over copied notes so each evaluator follows current guidance, examples, and platform transitions.

When starting a task:

1. Identify the exact behavior or quality promise, the repository evidence that can prove it, and the failure that must be caught.
2. Open Evaluation Best Practices first, then reduce the task to the smallest useful objective, case set, and scoring method.
3. Open the exact grader or worked example only when deterministic repository checks cannot settle the criterion.
4. These guides are unversioned; do not invent version segments, and check the deprecation route before depending on hosted Evals or grader APIs.

## Core Eval Design

### Evaluation Best Practices
Use when: Starting any evaluator and needing the authoritative overview of task-specific objectives, representative data, metrics, continuous evaluation, evaluator choices, and common anti-patterns.
Overview: https://developers.openai.com/api/docs/guides/evaluation-best-practices

### One Concrete Promise
Use when: Cutting a broad quality request down to one observable correctness, grounding, tool-use, format, business-rule, or migration promise.
Link: https://developers.openai.com/codex/use-cases/ai-app-evals#choose-what-to-evaluate

### Objective, Dataset, Metric, and Comparison
Use when: Turning operator intent and repository evidence into an explicit objective, representative cases, a measurable criterion, and a repeatable comparison loop.
Link: https://developers.openai.com/api/docs/guides/evaluation-best-practices#design-your-eval-process

### Eval Plan Before Implementation
Use when: Defining the target path, seed cases, assertions, fixtures, commands, and dependencies before implementation can bias the evaluator.
Link: https://developers.openai.com/codex/use-cases/ai-app-evals#ask-for-an-eval-plan

### Baseline Before Behavior Changes
Use when: Running the evaluator against the starting state, diagnosing brittle or vague assertions, and preserving real failures before changing behavior.
Link: https://developers.openai.com/codex/use-cases/ai-app-evals#implement-run-and-iterate

## Scoring and Rubrics

### Evaluator Types
Use when: Choosing among deterministic metrics, human review, pairwise comparison, reference-guided grading, and model judgment without forcing one scorer onto every task.
Reference: https://developers.openai.com/api/docs/guides/evaluation-best-practices#create-and-combine-different-types-of-evaluators

### Model-Judge Rubrics and Biases
Use when: A subjective criterion genuinely needs an LLM judge and the rubric must control position bias, verbosity bias, unclear criteria, and disagreement with humans.
Reference: https://developers.openai.com/api/docs/guides/evaluation-best-practices#llm-as-a-judge-and-model-graders

### Good, Fair, and Bad Grader Examples
Use when: Calibrating a grader with trusted answers at distinct quality levels and checking that its scores or ranking match human judgment.
Link: https://developers.openai.com/api/docs/guides/graders#how-to-write-grader-prompts
Link: https://developers.openai.com/api/docs/guides/graders#limitations-and-tips

## Cases and Calibration

### Representative and Adversarial Cases
Use when: Selecting normal, edge, ambiguous, long-context, conflicting-instruction, malformed-input, and other task-relevant cases without building a generic checklist.
Reference: https://developers.openai.com/api/docs/guides/evaluation-best-practices#handle-edge-cases

### Failure-Mode Analysis
Use when: Converting concrete bad outputs, bug reports, or review findings into a small taxonomy of failures worth measuring instead of guessing metrics first.
Link: https://developers.openai.com/cookbook/examples/evaluation/building_resilient_prompts_using_an_evaluation_flywheel#analyzing-prompt-effectiveness

### Human Calibration of an LLM Judge
Use when: Checking a model judge against expert pass and fail labels, avoiding misleading aggregate accuracy, and keeping a held-out test set.
Link: https://developers.openai.com/cookbook/examples/evaluation/building_resilient_prompts_using_an_evaluation_flywheel#aligning-your-llm-judge

## Good and Bad Worked Patterns

### Good Minimal Eval
Use when: Needing a concrete small example with one classification behavior, three representative inputs, human ground-truth labels, and one exact-match criterion.
Reference: https://developers.openai.com/api/docs/guides/evals#create-an-eval-for-a-task

### Bad Eval Anti-Patterns
Use when: Reviewing a draft evaluator for generic metrics, production-unrepresentative data, vibe-based acceptance, late evaluation, or uncalibrated automation.
Reference: https://developers.openai.com/api/docs/guides/evaluation-best-practices#what-are-evals

### Coding-Artifact Evaluator
Use when: Building a task-specific evaluator for code or repository artifacts that combines a short definition of good, structured findings, executable checks, and focused validation feedback.
Link: https://developers.openai.com/cookbook/examples/codex/build_iterative_repair_loops_with_codex#define-business-rules-and-issue-taxonomy
Link: https://developers.openai.com/cookbook/examples/codex/build_iterative_repair_loops_with_codex#validation-phase

### Evals Cookbook Index
Use when: The core design is settled and a domain-specific worked example is needed for agents, retrieval, tool use, structured extraction, multimodal behavior, or repair loops.
Full index: https://developers.openai.com/cookbook/topic/evals

## Common Failure and Lifecycle Risks

### Grader and Reward Hacking
Use when: Testing whether an implementation can earn a high score through a shortcut while still failing expert review or the operator's real intent.
Reference: https://developers.openai.com/api/docs/guides/graders#grader-hacking

### Hosted Evals Deprecation and Local Migration
Use when: Any design proposes the OpenAI Evals dashboard, Evals API, dataset workflow, or hosted grader workflow instead of a repository-local evaluator.
Link: https://developers.openai.com/api/docs/deprecations#2026-06-03-evals-platform
Link: https://developers.openai.com/cookbook/examples/evaluation/moving-from-openai-evals-to-promptfoo

## Agent Routing Notes

- Infer what matters from the operator's request and repository evidence; do not grade every task on a canned list of quality dimensions.
- Start with one claim about good behavior and the cheapest trustworthy evidence that can falsify it.
- Prefer existing tests, builds, linters, benchmarks, static checks, schemas, and direct artifact inspection before adding model judgment.
- Use binary, exact, or pairwise decisions when they express the real requirement; add a numeric score only when intermediate quality levels are meaningful.
- Give subjective graders explicit pass and fail examples, then test their agreement with human judgment before trusting them as a gate.
- Keep implementation and evaluator evidence distinct: a change does not pass because its author explains why it should pass.
- Treat a passing suite as evidence for its covered promises, not proof that the whole implementation is good; inspect uncovered risks and grader loopholes.
- Use the hosted Evals and grader pages for design patterns only; for new runnable suites, follow the current repository-local Codex and Promptfoo route.
