# Autoresearch for bettercodex

## Initial prompt

```text
andrej's autoresearch has still been occupying my mind for a while, https://github.com/karpathy/autoresearch

i've tried porting its principles over to bcodex but it was bad, we tried making a /loop but it wasnt good.

i really do think this is a gem of a repo though, I want you to break it down for me, what this autoresearch essentialy does, then we can rubber duck and bounce back ideas this session to seeing how we can carry it over to bcodex.

Keep responses concise. and we'll flesh out the @SPEC.md together as we discuss it in this session. The way you write it in the spec.md should be brief and not overexplained. and include my initial prompt (this prompt verbatim) Ok? lets begin
```

## Working model

`autoresearch` is not a loop feature. It is a controlled search system:

- one mutable surface: `train.py`;
- one protected environment and evaluator: `prepare.py`;
- one scalar objective: validation bits per byte;
- one fixed five-minute budget per candidate;
- baseline first;
- commit, run, measure, and log each experiment;
- keep improvements and reset failures;
- continue autonomously until interrupted; and
- the human edits research policy in `program.md`; the agent edits the experiment.

## Core insight

Autonomy comes from making evaluation, state, and rollback unambiguous—not from telling an agent to repeat. A generic `/loop` lacks the experiment contract that makes useful iteration possible.

## Translation hypothesis

bettercodex may need a reusable experiment contract rather than a universal loop:

- objective and acceptance rule;
- mutable and protected scope;
- evaluator and score extraction;
- trial budget and timeout;
- checkpoint and rollback;
- experiment ledger; and
- continuation and stopping policy.

## Chosen direction

An embedded bettercodex tool for autonomous optimization in arbitrary repositories. The campaign protocol is agnostic; each evaluator is task-specific.

## Required system

A frozen eval is central but insufficient. The closed system requires:

- a relevant, repeatable, gaming-resistant objective;
- frozen acceptance gates and protected files;
- a constrained mutable scope;
- one bounded candidate per worker;
- incumbent-versus-challenger evaluation;
- atomic accept or discard; and
- a durable experiment ledger.

“Unbiased” is an aspiration, not a guarantee. Use holdouts or adversarial checks where practical.

## Candidate architecture

1. The current agent studies the task and drafts a campaign contract plus its evaluator, using `docs/evals/MANIFEST.md` as design guidance.
2. The user reviews and freezes the evaluation suite.
3. A deterministic autoresearch runner inside bettercodex starts one fresh worker with the task, frozen manifest, current incumbent, and concise experiment ledger.
4. The worker produces exactly one candidate in a reusable isolated Git worktree.
5. The runner executes the evaluator, keeps an improvement, or resets a failure.
6. Repeat sequentially, with five candidates by default.

A fresh worker must not be stateless: accepted improvements accumulate, while the ledger prevents repeated failed ideas. “One attempt” means one candidate, not one edit or tool call.

## Eval integrity and threat model

- The runner records the canonical baseline and frozen evaluator before workers start.
- A worker returns only a candidate diff; its claimed test results and score are ignored.
- The runner rejects out-of-scope changes, applies the allowed diff to a clean incumbent, and executes the canonical evaluator itself.
- Prefer behavior-level, adversarial, randomized, or held-out cases where useful.
- The user reviews the final winner before it leaves the campaign worktree.

An unsandboxed same-user shell is not a security boundary. V1 resists eval tampering and accidental reward hacking, but cannot contain a deliberately hostile worker without sandboxing.

## V1 threat model

Workers are cooperative but may exploit weak metrics while optimizing. The runner therefore owns scoring and validates candidate diffs against a clean incumbent. Public checks guide workers; optional held-out cases catch shortcuts. Deliberately hostile workers and OS-level containment are out of scope.

## V1 simplicity constraints

- one scalar score plus hard pass/fail gates;
- one user approval is the only freeze gate;
- one deterministic runner and one worker at a time;
- one campaign branch and reusable worktree;
- the same model and reasoning effort for every worker; and
- no critic agent, teams, parallel candidates, tmux integration, or plugin framework.

Tasks without a meaningful repeatable evaluator are unsupported. Noisy or multi-dimensional selection is deferred.

## Lessons from pi-autoresearch

Retain:

- separate generic campaign machinery from task-specific research instructions;
- keep one campaign directory as the source of truth;
- persist a concise contract and append-only JSONL experiment ledger;
- parse structured metric output from the evaluator;
- keep correctness gates distinct from the optimization score; and
- rehydrate fresh workers from persisted state, not parent conversation history.

Do not retain:

- an agent-editable evaluator;
- agent-reported scores or keep/discard decisions;
- one endlessly auto-resumed agent;
- broad `git add -A` mutation handling; or
- hooks, dashboards, confidence heuristics, and finalization machinery in V1.

The minimal campaign state is a contract, a frozen evaluator, and a ledger. Fast noisy workloads may stabilize themselves inside the evaluator, such as by reporting a median.
